//! SQLite operation journal for deployment and restore recovery.
//!
//! This store owns durable plans, the exclusive per-root lease, operation and
//! step phases, and committed generations in the existing `catalog.sqlite3`.
//! Evidence is a versioned portable encoding checked on every read; no caller
//! path or native diagnostic is stored.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use marduk_core::deployment::{
    BoundTarget, DeployError, DeploymentPlan, FileIntent, GenerationId, OperationId, OperationKind,
    OperationStatus, Phase, PlanId, PreState, SourceRef,
};
use marduk_core::ids::{InstalledRevisionId, ProfileId, ProfileRevision};
use marduk_core::recovery::{
    Artifact, BackupRef, BackupSlot, Claim, Evidence, Finalize, GenerationRecord, Journal,
    Observation, PreImage, PreparedStep, RootState, StageRef, StepDirection, StepOutput, StepPhase,
    StepRecord, StepSpec,
};
use marduk_core::root_identity::{
    FileMetadata, Fingerprint, NativeIdentity, RootError, RootIdentity, UnsupportedReason,
};
use marduk_core::targets::{RelativePath, RootRole, TargetFile};
use marduk_platform::{GateError, WriterGate, WriterGuard};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

const CATALOG_FILE: &str = "catalog.sqlite3";
const LOCK_FILE: &str = "library.lock";
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const MIGRATION_VERSION: i64 = 2;
const SCHEMA_V1: &str = include_str!("../migrations/0001_library.sql");
const SCHEMA_V2: &str = include_str!("../migrations/0002_operations.sql");

pub struct OperationJournal {
    path: PathBuf,
    connection: Connection,
    gate: WriterGate,
}

impl OperationJournal {
    /// Open (and migrate) the shared catalog under the application data root.
    pub fn open(app_data: &Path) -> Result<Self, DeployError> {
        fs::create_dir_all(app_data).map_err(|_| DeployError::Io)?;
        let gate = WriterGate::open(app_data.join(LOCK_FILE)).map_err(gate_error)?;
        let path = app_data.join(CATALOG_FILE);
        let mut connection = Connection::open(&path).map_err(|_| DeployError::Io)?;

        let _guard = gate.try_lock().map_err(gate_error)?;
        configure(&connection)?;
        migrate(&mut connection)?;
        seed_profile(&mut connection)?;
        verify_pragmas(&connection)?;

        Ok(Self {
            path,
            connection,
            gate,
        })
    }

    /// Persist the verified Setup pair. A pending operation keeps its recorded
    /// locator; a different pair cannot silently replace the lease.
    pub fn select_verified_installation(
        &mut self,
        locator: &Path,
        installation: &RootIdentity,
        data: &RootIdentity,
    ) -> Result<(), DeployError> {
        let _guard = self.lock()?;
        let installation_blob = encode_root(installation);
        let data_blob = encode_root(data);
        let location = encode_locator(locator);

        let transaction = self.immediate()?;
        let lease: Option<(Vec<u8>, Option<i64>, Option<i64>)> = transaction
            .query_row(
                "SELECT installation, pending_op, active_generation FROM root_leases \
                 WHERE data = ?1",
                [data_blob.as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|_| DeployError::Io)?;

        let keep_location = match lease {
            Some((recorded, pending, active)) => {
                if recorded != installation_blob {
                    return Err(if pending.is_some() {
                        DeployError::Busy
                    } else if active.is_some() {
                        DeployError::ActiveDeployment
                    } else {
                        DeployError::TargetUnavailable
                    });
                }
                pending.is_some()
            }
            None => false,
        };

        transaction
            .execute(
                "UPDATE installations SET selected = 0 WHERE selected = 1",
                [],
            )
            .map_err(|_| DeployError::Io)?;
        if keep_location {
            transaction
                .execute(
                    "INSERT INTO installations (selected, location, installation, data) \
                     VALUES (1, ?1, ?2, ?3) \
                     ON CONFLICT (installation, data) DO UPDATE SET selected = 1",
                    params![
                        location.as_slice(),
                        installation_blob.as_slice(),
                        data_blob.as_slice()
                    ],
                )
                .map_err(|_| DeployError::Io)?;
        } else {
            transaction
                .execute(
                    "INSERT INTO installations (selected, location, installation, data) \
                     VALUES (1, ?1, ?2, ?3) \
                     ON CONFLICT (installation, data) \
                     DO UPDATE SET selected = 1, location = excluded.location",
                    params![
                        location.as_slice(),
                        installation_blob.as_slice(),
                        data_blob.as_slice()
                    ],
                )
                .map_err(|_| DeployError::Io)?;
        }
        transaction.commit().map_err(|_| DeployError::Io)
    }

    fn lock(&self) -> Result<WriterGuard, DeployError> {
        self.gate.try_lock().map_err(gate_error)
    }

    fn immediate(&mut self) -> Result<Transaction<'_>, DeployError> {
        self.connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| DeployError::Io)
    }

    fn read_plan(&self, id: PlanId) -> Result<Option<DeploymentPlan>, DeployError> {
        let plan_value = sql_id(id.get())?;
        let row: Option<(i64, i64, Vec<u8>, Vec<u8>)> = self
            .connection
            .query_row(
                "SELECT profile_id, revision, installation, data \
                 FROM deployment_plans WHERE id = ?1",
                [plan_value],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(|_| DeployError::Io)?;
        let Some((profile, revision, installation, data)) = row else {
            return Ok(None);
        };

        let intents = read_intents(&self.connection, plan_value)?;
        Ok(Some(DeploymentPlan {
            id,
            profile: ProfileId::new(nonnegative(profile)?),
            revision: ProfileRevision::new(nonnegative(revision)?),
            installation: decode_root(&installation)?,
            data: decode_root(&data)?,
            intents,
        }))
    }

    fn read_steps(&self, op: OperationId) -> Result<Vec<StepRecord>, DeployError> {
        read_steps(&self.connection, op)
    }
}

impl Journal for OperationJournal {
    fn reopen(&mut self) -> Result<(), DeployError> {
        let connection = Connection::open(&self.path).map_err(|_| DeployError::Io)?;
        configure(&connection)?;
        verify_pragmas(&connection)?;
        self.connection = connection;
        Ok(())
    }

    fn save_plan(
        &mut self,
        profile: ProfileId,
        revision: ProfileRevision,
        installation: RootIdentity,
        data: RootIdentity,
        intents: Vec<FileIntent>,
    ) -> Result<DeploymentPlan, DeployError> {
        let _guard = self.lock()?;
        let installation_blob = encode_root(&installation);
        let data_blob = encode_root(&data);

        let transaction = self.immediate()?;
        let installation_id = ensure_installation(&transaction, &installation_blob, &data_blob)?;
        transaction
            .execute(
                "INSERT INTO deployment_plans \
                 (installation_id, profile_id, revision, installation, data) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    installation_id,
                    sql_id(profile.get())?,
                    sql_nonnegative(revision.get())?,
                    installation_blob.as_slice(),
                    data_blob.as_slice()
                ],
            )
            .map_err(|_| DeployError::Io)?;
        let plan_value = transaction.last_insert_rowid();
        let id = PlanId::try_from_u64(row_id(plan_value)?)?;

        for (ordinal, intent) in intents.iter().enumerate() {
            let ordinal = i64::try_from(ordinal).map_err(|_| DeployError::Io)?;
            write_intent(&transaction, plan_value, ordinal, intent)?;
        }
        transaction.commit().map_err(|_| DeployError::Io)?;

        Ok(DeploymentPlan {
            id,
            profile,
            revision,
            installation,
            data,
            intents,
        })
    }

    fn plan(&mut self, id: PlanId) -> Result<Option<DeploymentPlan>, DeployError> {
        self.read_plan(id)
    }

    fn claimed(&mut self, id: PlanId) -> Result<Option<OperationStatus>, DeployError> {
        let plan_value = sql_id(id.get())?;
        let op: Option<i64> = self
            .connection
            .query_row(
                "SELECT id FROM operations WHERE plan_id = ?1",
                [plan_value],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| DeployError::Io)?;
        match op {
            Some(op) => read_status(&self.connection, OperationId::try_from_u64(row_id(op)?)?),
            None => Ok(None),
        }
    }

    fn root_state(&mut self, data: &RootIdentity) -> Result<RootState, DeployError> {
        let data_blob = encode_root(data);
        let row: Option<(Option<i64>, Option<i64>)> = self
            .connection
            .query_row(
                "SELECT pending_op, active_generation FROM root_leases WHERE data = ?1",
                [data_blob.as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|_| DeployError::Io)?;
        let Some((pending, active)) = row else {
            return Ok(RootState {
                pending: None,
                active: None,
            });
        };
        Ok(RootState {
            pending: pending
                .map(row_id)
                .transpose()?
                .map(OperationId::try_from_u64)
                .transpose()?,
            active: active
                .map(row_id)
                .transpose()?
                .map(GenerationId::try_from_u64)
                .transpose()?,
        })
    }

    fn claim(&mut self, claim: Claim) -> Result<OperationId, DeployError> {
        let _guard = self.lock()?;
        let transaction = self.immediate()?;

        let (installation_blob, data_blob, installation_id, kind, plan_id, from_generation) =
            match &claim {
                Claim::Deploy(plan) => {
                    let plan_value = sql_id(plan.id.get())?;
                    let stored: Option<(Vec<u8>, Vec<u8>)> = transaction
                        .query_row(
                            "SELECT installation, data FROM deployment_plans WHERE id = ?1",
                            [plan_value],
                            |row| Ok((row.get(0)?, row.get(1)?)),
                        )
                        .optional()
                        .map_err(|_| DeployError::Io)?;
                    let Some((stored_installation, stored_data)) = stored else {
                        return Err(DeployError::StalePlan);
                    };
                    if stored_installation != encode_root(&plan.installation)
                        || stored_data != encode_root(&plan.data)
                    {
                        return Err(DeployError::StalePlan);
                    }
                    if let Some(op) = operation_for_plan(&transaction, plan_value)? {
                        return Ok(op);
                    }
                    (
                        encode_root(&plan.installation),
                        encode_root(&plan.data),
                        ensure_installation(
                            &transaction,
                            &encode_root(&plan.installation),
                            &encode_root(&plan.data),
                        )?,
                        "deploy",
                        Some(plan_value),
                        None,
                    )
                }
                Claim::Restore {
                    installation,
                    data,
                    generation,
                } => {
                    let generation_value = sql_id(generation.get())?;
                    let stored: Option<(Vec<u8>, Vec<u8>)> = transaction
                        .query_row(
                            "SELECT data, installation FROM generations WHERE id = ?1",
                            [generation_value],
                            |row| Ok((row.get(0)?, row.get(1)?)),
                        )
                        .optional()
                        .map_err(|_| DeployError::Io)?;
                    let Some((stored_data, stored_installation)) = stored else {
                        return Err(DeployError::UnknownOperation);
                    };
                    if stored_data != encode_root(data)
                        || stored_installation != encode_root(installation)
                    {
                        return Err(DeployError::TargetUnavailable);
                    }
                    (
                        encode_root(installation),
                        encode_root(data),
                        ensure_installation(
                            &transaction,
                            &encode_root(installation),
                            &encode_root(data),
                        )?,
                        "restore",
                        None,
                        Some(generation_value),
                    )
                }
            };

        let lease = load_or_create_lease(&transaction, &data_blob, &installation_blob)?;
        if lease.installation != installation_blob {
            return Err(DeployError::TargetUnavailable);
        }
        if lease.pending.is_some() {
            return Err(DeployError::Busy);
        }

        match kind {
            "deploy" => {
                if lease.active.is_some() {
                    return Err(DeployError::ActiveDeployment);
                }
            }
            "restore" => {
                if lease.active != from_generation {
                    return Err(DeployError::NoActiveDeployment);
                }
            }
            _ => return Err(DeployError::Io),
        }

        transaction
            .execute(
                "INSERT INTO operations \
                 (plan_id, installation_id, installation, data, kind, phase, from_generation) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 'preparing', ?6)",
                params![
                    plan_id,
                    installation_id,
                    installation_blob.as_slice(),
                    data_blob.as_slice(),
                    kind,
                    from_generation
                ],
            )
            .map_err(|_| DeployError::Io)?;
        let op_value = transaction.last_insert_rowid();
        let op = OperationId::try_from_u64(row_id(op_value)?)?;

        let changed = transaction
            .execute(
                "UPDATE root_leases SET pending_op = ?1 \
                 WHERE data = ?2 AND pending_op IS NULL",
                params![op_value, data_blob.as_slice()],
            )
            .map_err(|_| DeployError::Io)?;
        if changed != 1 {
            return Err(DeployError::Busy);
        }
        transaction.commit().map_err(|_| DeployError::Io)?;
        Ok(op)
    }

    fn seal(&mut self, op: OperationId, steps: Vec<PreparedStep>) -> Result<(), DeployError> {
        let _guard = self.lock()?;
        let transaction = self.immediate()?;
        let row = load_operation(&transaction, op)?.ok_or(DeployError::UnknownOperation)?;
        if row.phase != Phase::Preparing || !pending_is(&transaction, &row.data, op)? {
            return Err(DeployError::Io);
        }
        let op_value = sql_id(op.get())?;

        match row.kind {
            OperationKind::Deploy => {
                let plan_value: i64 = transaction
                    .query_row(
                        "SELECT plan_id FROM operations WHERE id = ?1",
                        [op_value],
                        |row| row.get(0),
                    )
                    .map_err(|_| DeployError::Io)?;
                let expected = read_plan_paths(&transaction, plan_value)?;
                if expected.len() != steps.len() {
                    return Err(DeployError::Io);
                }
                for (index, (path, step)) in expected.iter().zip(steps.iter()).enumerate() {
                    if u32::try_from(index).map_err(|_| DeployError::Io)? != step.ordinal
                        || step.spec.target.target.relative.as_str() != path
                    {
                        return Err(DeployError::Io);
                    }
                }
            }
            OperationKind::Restore => {
                let from_generation: i64 = transaction
                    .query_row(
                        "SELECT from_generation FROM operations WHERE id = ?1",
                        [op_value],
                        |row| row.get(0),
                    )
                    .map_err(|_| DeployError::Io)?;
                let deploy: i64 = transaction
                    .query_row(
                        "SELECT deploy_op FROM generations WHERE id = ?1",
                        [from_generation],
                        |row| row.get(0),
                    )
                    .map_err(|_| DeployError::Io)?;
                let expected: i64 = transaction
                    .query_row(
                        "SELECT count(*) FROM operation_steps WHERE operation_id = ?1",
                        [deploy],
                        |row| row.get(0),
                    )
                    .map_err(|_| DeployError::Io)?;
                if usize::try_from(expected).map_err(|_| DeployError::Io)? != steps.len() {
                    return Err(DeployError::Io);
                }
            }
        }

        let mut ordinals = BTreeSet::new();
        for step in &steps {
            if step.op != op
                || step.direction != StepDirection::Forward
                || !ordinals.insert(step.ordinal)
            {
                return Err(DeployError::Io);
            }
            if !valid_pre_image(&step.spec.original)
                || !valid_pre_image(&step.step_pre)
                || step.spec.original.backup.is_some_and(|at| {
                    at != BackupRef::from_record(
                        op,
                        step.ordinal,
                        StepDirection::Forward,
                        BackupSlot::Original,
                    )
                })
                || step.step_pre.backup.is_some_and(|at| {
                    at != BackupRef::from_record(
                        op,
                        step.ordinal,
                        StepDirection::Forward,
                        BackupSlot::Step,
                    )
                })
                || step.staged.as_ref().is_some_and(|(at, _)| {
                    *at != StageRef::from_record(op, step.ordinal, StepDirection::Forward)
                })
                || step.retain
                    != BackupRef::from_record(
                        op,
                        step.ordinal,
                        StepDirection::Forward,
                        BackupSlot::Displaced,
                    )
            {
                return Err(DeployError::Io);
            }

            transaction
                .execute(
                    "INSERT INTO operation_steps \
                     (operation_id, ordinal, target, output, original_observation, \
                      original_backup, step_pre_observation, step_pre_backup, stage_ref, \
                      stage_fingerprint, displaced_ref, phase) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'ready')",
                    params![
                        op_value,
                        i64::from(step.ordinal),
                        encode_bound(&step.spec.target),
                        encode_output(&step.spec.output),
                        encode_pre_state(&step.spec.original.observation),
                        step.spec.original.backup.map(encode_backup),
                        encode_pre_state(&step.step_pre.observation),
                        step.step_pre.backup.map(encode_backup),
                        step.staged.as_ref().map(|(at, _)| encode_stage(*at)),
                        step.staged.as_ref().map(|(_, fp)| encode_fingerprint(fp)),
                        encode_backup(step.retain)
                    ],
                )
                .map_err(|_| DeployError::Io)?;
        }

        let changed = transaction
            .execute(
                "UPDATE operations SET phase = 'prepared' \
                 WHERE id = ?1 AND phase = 'preparing'",
                [op_value],
            )
            .map_err(|_| DeployError::Io)?;
        if changed != 1 {
            return Err(DeployError::Io);
        }
        transaction.commit().map_err(|_| DeployError::Io)
    }

    fn set_phase(&mut self, op: OperationId, from: Phase, to: Phase) -> Result<(), DeployError> {
        let _guard = self.lock()?;
        if to.is_terminal() {
            return Err(DeployError::Io);
        }
        let changed = self
            .connection
            .execute(
                "UPDATE operations SET phase = ?1 WHERE id = ?2 AND phase = ?3 \
                 AND EXISTS (SELECT 1 FROM root_leases WHERE pending_op = ?2)",
                params![phase_text(to), sql_id(op.get())?, phase_text(from)],
            )
            .map_err(|_| DeployError::Io)?;
        if changed != 1 {
            return Err(DeployError::Io);
        }
        Ok(())
    }

    fn record_step(
        &mut self,
        op: OperationId,
        ordinal: u32,
        from: StepPhase,
        to: StepPhase,
        evidence: Option<Evidence>,
    ) -> Result<(), DeployError> {
        if matches!(to, StepPhase::Applied | StepPhase::Undone) && evidence.is_none() {
            return Err(DeployError::Io);
        }
        let _guard = self.lock()?;
        let evidence = evidence.map(|evidence| encode_evidence(&evidence));
        let changed = self
            .connection
            .execute(
                "UPDATE operation_steps SET phase = ?1, observed = COALESCE(?2, observed) \
                 WHERE operation_id = ?3 AND ordinal = ?4 AND phase = ?5 \
                 AND EXISTS (SELECT 1 FROM root_leases WHERE pending_op = ?3)",
                params![
                    step_phase_text(to),
                    evidence.as_deref(),
                    sql_id(op.get())?,
                    i64::from(ordinal),
                    step_phase_text(from)
                ],
            )
            .map_err(|_| DeployError::Io)?;
        if changed != 1 {
            return Err(DeployError::Io);
        }
        Ok(())
    }

    fn block(&mut self, op: OperationId, error: Option<DeployError>) -> Result<(), DeployError> {
        let _guard = self.lock()?;
        let changed = self
            .connection
            .execute(
                "UPDATE operations SET blocker = ?1 WHERE id = ?2",
                params![error.map(|error| error.as_str()), sql_id(op.get())?],
            )
            .map_err(|_| DeployError::Io)?;
        if changed != 1 {
            return Err(DeployError::UnknownOperation);
        }
        Ok(())
    }

    fn finish(&mut self, op: OperationId, how: Finalize) -> Result<OperationStatus, DeployError> {
        let _guard = self.lock()?;
        let transaction = self.immediate()?;
        let row = load_operation(&transaction, op)?.ok_or(DeployError::UnknownOperation)?;
        if !pending_is(&transaction, &row.data, op)? {
            return Err(DeployError::UnknownOperation);
        }
        let op_value = sql_id(op.get())?;
        let data_blob = row.data.clone();

        match how {
            Finalize::DeployCommitted => {
                if row.kind != OperationKind::Deploy
                    || row.phase != Phase::Applying
                    || row.blocker.is_some()
                    || !all_steps_applied(&transaction, op_value)?
                {
                    return Err(DeployError::Io);
                }
                transaction
                    .execute(
                        "INSERT INTO generations (deploy_op, data, installation) \
                         VALUES (?1, ?2, ?3)",
                        params![op_value, row.data.as_slice(), row.installation.as_slice()],
                    )
                    .map_err(|_| DeployError::Io)?;
                let generation_value = transaction.last_insert_rowid();
                let changed = transaction
                    .execute(
                        "UPDATE operations SET phase = 'completed', result_generation = ?1 \
                         WHERE id = ?2 AND phase = 'applying'",
                        params![generation_value, op_value],
                    )
                    .map_err(|_| DeployError::Io)?;
                if changed != 1 {
                    return Err(DeployError::Io);
                }
                let changed = transaction
                    .execute(
                        "UPDATE root_leases SET pending_op = NULL, active_generation = ?1 \
                         WHERE data = ?2 AND pending_op = ?3 AND active_generation IS NULL",
                        params![generation_value, data_blob.as_slice(), op_value],
                    )
                    .map_err(|_| DeployError::Io)?;
                if changed != 1 {
                    return Err(DeployError::Io);
                }
            }
            Finalize::RestoreCompleted => {
                if row.kind != OperationKind::Restore
                    || row.phase != Phase::Restoring
                    || row.blocker.is_some()
                    || !all_steps_applied(&transaction, op_value)?
                {
                    return Err(DeployError::Io);
                }
                let changed = transaction
                    .execute(
                        "UPDATE operations SET phase = 'completed' \
                         WHERE id = ?1 AND phase = 'restoring'",
                        [op_value],
                    )
                    .map_err(|_| DeployError::Io)?;
                if changed != 1 {
                    return Err(DeployError::Io);
                }
                let changed = transaction
                    .execute(
                        "UPDATE root_leases SET pending_op = NULL, active_generation = NULL \
                         WHERE data = ?1 AND pending_op = ?2",
                        params![data_blob.as_slice(), op_value],
                    )
                    .map_err(|_| DeployError::Io)?;
                if changed != 1 {
                    return Err(DeployError::Io);
                }
            }
            Finalize::RolledBack => {
                if row.kind != OperationKind::Deploy
                    || !matches!(row.phase, Phase::Applying | Phase::RollingBack)
                {
                    return Err(DeployError::Io);
                }
                let changed = transaction
                    .execute(
                        "UPDATE operations SET phase = 'rolled_back', blocker = NULL \
                         WHERE id = ?1 AND phase IN ('applying', 'rolling_back')",
                        [op_value],
                    )
                    .map_err(|_| DeployError::Io)?;
                if changed != 1 {
                    return Err(DeployError::Io);
                }
                clear_pending(&transaction, &data_blob, op)?;
            }
            Finalize::Cancelled => {
                if !matches!(row.phase, Phase::Preparing | Phase::Prepared) {
                    return Err(DeployError::Io);
                }
                let changed = transaction
                    .execute(
                        "UPDATE operations SET phase = 'cancelled', blocker = NULL \
                         WHERE id = ?1 AND phase IN ('preparing', 'prepared')",
                        [op_value],
                    )
                    .map_err(|_| DeployError::Io)?;
                if changed != 1 {
                    return Err(DeployError::Io);
                }
                clear_pending(&transaction, &data_blob, op)?;
            }
        }

        let status = read_status(&transaction, op)?.ok_or(DeployError::UnknownOperation)?;
        transaction.commit().map_err(|_| DeployError::Io)?;
        Ok(status)
    }

    fn status(&mut self, op: OperationId) -> Result<Option<OperationStatus>, DeployError> {
        read_status(&self.connection, op)
    }

    fn steps(&mut self, op: OperationId) -> Result<Vec<StepRecord>, DeployError> {
        self.read_steps(op)
    }

    fn generation(&mut self, id: GenerationId) -> Result<GenerationRecord, DeployError> {
        let generation_value = sql_id(id.get())?;
        let row: Option<(i64, Vec<u8>, Vec<u8>)> = self
            .connection
            .query_row(
                "SELECT deploy_op, data, installation FROM generations WHERE id = ?1",
                [generation_value],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|_| DeployError::Io)?;
        let Some((deploy, data, installation)) = row else {
            return Err(DeployError::UnknownOperation);
        };
        let deploy = OperationId::try_from_u64(row_id(deploy)?)?;

        let operation =
            load_operation(&self.connection, deploy)?.ok_or(DeployError::UnknownOperation)?;
        if operation.data != data || operation.installation != installation {
            return Err(DeployError::Io);
        }
        if operation.kind != OperationKind::Deploy || operation.phase != Phase::Completed {
            return Err(DeployError::Io);
        }

        Ok(GenerationRecord {
            id,
            deploy,
            steps: self.read_steps(deploy)?,
        })
    }

    fn record_undo(
        &mut self,
        op: OperationId,
        ordinal: u32,
        undo: PreparedStep,
    ) -> Result<(), DeployError> {
        let _guard = self.lock()?;
        let transaction = self.immediate()?;
        let op_value = sql_id(op.get())?;
        let data: Vec<u8> = transaction
            .query_row(
                "SELECT data FROM operations WHERE id = ?1",
                [op_value],
                |row| row.get(0),
            )
            .map_err(|_| DeployError::Io)?;
        if !pending_is(&transaction, &data, op)? {
            return Err(DeployError::UnknownOperation);
        }
        let row: Option<(Vec<u8>, Option<Vec<u8>>)> = transaction
            .query_row(
                "SELECT displaced_ref, undo FROM operation_steps \
                 WHERE operation_id = ?1 AND ordinal = ?2",
                params![op_value, i64::from(ordinal)],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|_| DeployError::Io)?;
        let Some((displaced, existing)) = row else {
            return Err(DeployError::UnknownOperation);
        };
        if existing.is_some()
            || undo.op != op
            || undo.ordinal != ordinal
            || undo.direction != StepDirection::Undo
            || decode_backup(&displaced)? != undo.retain
        {
            return Err(DeployError::Io);
        }
        let encoded = encode_prepared(&undo);
        let changed = transaction
            .execute(
                "UPDATE operation_steps SET undo = ?1 \
                 WHERE operation_id = ?2 AND ordinal = ?3 AND undo IS NULL",
                params![encoded.as_slice(), op_value, i64::from(ordinal)],
            )
            .map_err(|_| DeployError::Io)?;
        if changed != 1 {
            return Err(DeployError::Io);
        }
        transaction.commit().map_err(|_| DeployError::Io)
    }
}

fn configure(connection: &Connection) -> Result<(), DeployError> {
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(|_| DeployError::Io)?;
    connection
        .pragma_update(None, "foreign_keys", 1)
        .map_err(|_| DeployError::Io)?;
    connection
        .pragma_update(None, "synchronous", 2)
        .map_err(|_| DeployError::Io)?;
    let _: String = connection
        .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))
        .map_err(|_| DeployError::Io)?;
    Ok(())
}

fn migrate(connection: &mut Connection) -> Result<(), DeployError> {
    let version = user_version(connection)?;
    if version < 0 || version > MIGRATION_VERSION {
        return Err(DeployError::Io);
    }

    if version < 1 {
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| DeployError::Io)?;
        if user_version(&transaction)? != 0 {
            return Err(DeployError::Io);
        }
        transaction
            .execute_batch(SCHEMA_V1)
            .map_err(|_| DeployError::Io)?;
        transaction
            .pragma_update(None, "user_version", 1)
            .map_err(|_| DeployError::Io)?;
        transaction.commit().map_err(|_| DeployError::Io)?;
    }

    if user_version(connection)? < MIGRATION_VERSION {
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| DeployError::Io)?;
        if user_version(&transaction)? != 1 {
            return Err(DeployError::Io);
        }
        transaction
            .execute_batch(SCHEMA_V2)
            .map_err(|_| DeployError::Io)?;
        transaction
            .pragma_update(None, "user_version", MIGRATION_VERSION)
            .map_err(|_| DeployError::Io)?;
        transaction.commit().map_err(|_| DeployError::Io)?;
    }

    if user_version(connection)? != MIGRATION_VERSION {
        return Err(DeployError::Io);
    }
    Ok(())
}

fn seed_profile(connection: &mut Connection) -> Result<(), DeployError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| DeployError::Io)?;
    transaction
        .execute(
            "INSERT INTO profiles (id, revision) VALUES (1, 0) ON CONFLICT (id) DO NOTHING",
            [],
        )
        .map_err(|_| DeployError::Io)?;
    transaction.commit().map_err(|_| DeployError::Io)
}

fn verify_pragmas(connection: &Connection) -> Result<(), DeployError> {
    let foreign_keys: i64 = connection
        .pragma_query_value(None, "foreign_keys", |row| row.get(0))
        .map_err(|_| DeployError::Io)?;
    let journal_mode: String = connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .map_err(|_| DeployError::Io)?;
    let synchronous: i64 = connection
        .pragma_query_value(None, "synchronous", |row| row.get(0))
        .map_err(|_| DeployError::Io)?;
    if foreign_keys != 1 || !journal_mode.eq_ignore_ascii_case("wal") || synchronous != 2 {
        return Err(DeployError::Io);
    }
    Ok(())
}

fn user_version(connection: &Connection) -> Result<i64, DeployError> {
    connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|_| DeployError::Io)
}

fn gate_error(error: GateError) -> DeployError {
    match error {
        GateError::Busy => DeployError::Busy,
        GateError::Unavailable | GateError::Io => DeployError::Io,
    }
}

fn ensure_installation(
    connection: &Connection,
    installation: &[u8],
    data: &[u8],
) -> Result<i64, DeployError> {
    let existing: Option<i64> = connection
        .query_row(
            "SELECT id FROM installations WHERE installation = ?1 AND data = ?2",
            params![installation, data],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| DeployError::Io)?;
    if let Some(id) = existing {
        return Ok(id);
    }
    connection
        .execute(
            "INSERT INTO installations (selected, location, installation, data) \
             VALUES (0, ?1, ?2, ?3)",
            params![empty_locator(), installation, data],
        )
        .map_err(|_| DeployError::Io)?;
    Ok(connection.last_insert_rowid())
}

struct LeaseRow {
    installation: Vec<u8>,
    pending: Option<i64>,
    active: Option<i64>,
}

fn load_or_create_lease(
    connection: &Connection,
    data: &[u8],
    installation: &[u8],
) -> Result<LeaseRow, DeployError> {
    let row: Option<(Vec<u8>, Option<i64>, Option<i64>)> = connection
        .query_row(
            "SELECT installation, pending_op, active_generation FROM root_leases \
             WHERE data = ?1",
            [data],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|_| DeployError::Io)?;
    if let Some((installation, pending, active)) = row {
        return Ok(LeaseRow {
            installation,
            pending,
            active,
        });
    }
    connection
        .execute(
            "INSERT INTO root_leases (data, installation) VALUES (?1, ?2)",
            params![data, installation],
        )
        .map_err(|_| DeployError::Io)?;
    Ok(LeaseRow {
        installation: installation.to_vec(),
        pending: None,
        active: None,
    })
}

struct OperationRow {
    kind: OperationKind,
    phase: Phase,
    from_generation: Option<i64>,
    result_generation: Option<i64>,
    blocker: Option<String>,
    installation: Vec<u8>,
    data: Vec<u8>,
}

fn load_operation(
    connection: &Connection,
    op: OperationId,
) -> Result<Option<OperationRow>, DeployError> {
    let row: Option<(
        String,
        String,
        Option<i64>,
        Option<i64>,
        Option<String>,
        Vec<u8>,
        Vec<u8>,
    )> = connection
        .query_row(
            "SELECT kind, phase, from_generation, result_generation, blocker, \
                 installation, data FROM operations WHERE id = ?1",
            [sql_id(op.get())?],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .optional()
        .map_err(|_| DeployError::Io)?;
    let Some((kind, phase, from_generation, result_generation, blocker, installation, data)) = row
    else {
        return Ok(None);
    };
    Ok(Some(OperationRow {
        kind: parse_kind(&kind)?,
        phase: parse_phase(&phase)?,
        from_generation,
        result_generation,
        blocker,
        installation,
        data,
    }))
}

fn read_status(
    connection: &Connection,
    op: OperationId,
) -> Result<Option<OperationStatus>, DeployError> {
    let Some(row) = load_operation(connection, op)? else {
        return Ok(None);
    };
    let retained = read_retained(connection, op)?;
    let generation = match (row.kind, row.phase) {
        (OperationKind::Deploy, Phase::Completed) => row.result_generation,
        (OperationKind::Restore, Phase::Completed) => None,
        _ => row.result_generation.or(row.from_generation),
    };
    Ok(Some(OperationStatus {
        id: op,
        kind: row.kind,
        phase: row.phase,
        generation: generation
            .map(row_id)
            .transpose()?
            .map(GenerationId::try_from_u64)
            .transpose()?,
        blocker: row.blocker.as_deref().map(parse_error).transpose()?,
        retained,
    }))
}

fn read_retained(connection: &Connection, op: OperationId) -> Result<Vec<BackupRef>, DeployError> {
    let rows = {
        let mut statement = connection
            .prepare(
                "SELECT original_backup, step_pre_backup FROM operation_steps \
                 WHERE operation_id = ?1 ORDER BY ordinal ASC",
            )
            .map_err(|_| DeployError::Io)?;
        let rows = statement
            .query_map([sql_id(op.get())?], |row| {
                Ok((
                    row.get::<_, Option<Vec<u8>>>(0)?,
                    row.get::<_, Option<Vec<u8>>>(1)?,
                ))
            })
            .map_err(|_| DeployError::Io)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|_| DeployError::Io)?
    };
    let mut retained = Vec::new();
    for (original, step) in rows {
        if let Some(blob) = original {
            retained.push(decode_backup(&blob)?);
        }
        if let Some(blob) = step {
            retained.push(decode_backup(&blob)?);
        }
    }
    Ok(retained)
}

fn read_steps(connection: &Connection, op: OperationId) -> Result<Vec<StepRecord>, DeployError> {
    let rows = {
        let mut statement = connection
            .prepare(
                "SELECT operation_id, ordinal, target, output, original_observation, \
                 original_backup, step_pre_observation, step_pre_backup, stage_ref, \
                 stage_fingerprint, displaced_ref, phase, observed, undo \
                 FROM operation_steps WHERE operation_id = ?1 ORDER BY ordinal ASC",
            )
            .map_err(|_| DeployError::Io)?;
        let rows = statement
            .query_map([sql_id(op.get())?], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                    row.get::<_, Option<Vec<u8>>>(5)?,
                    row.get::<_, Vec<u8>>(6)?,
                    row.get::<_, Option<Vec<u8>>>(7)?,
                    row.get::<_, Option<Vec<u8>>>(8)?,
                    row.get::<_, Option<Vec<u8>>>(9)?,
                    row.get::<_, Vec<u8>>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, Option<Vec<u8>>>(12)?,
                    row.get::<_, Option<Vec<u8>>>(13)?,
                ))
            })
            .map_err(|_| DeployError::Io)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|_| DeployError::Io)?
    };

    let mut steps = Vec::with_capacity(rows.len());
    for (
        operation,
        ordinal,
        target,
        output,
        original_observation,
        original_backup,
        step_pre_observation,
        step_pre_backup,
        stage_ref,
        stage_fingerprint,
        displaced_ref,
        phase,
        observed,
        undo,
    ) in rows
    {
        let operation = OperationId::try_from_u64(row_id(operation)?)?;
        if operation != op {
            return Err(DeployError::Io);
        }
        let ordinal = u32::try_from(ordinal).map_err(|_| DeployError::Io)?;
        let original_backup = original_backup.as_deref().map(decode_backup).transpose()?;
        if original_backup.is_some_and(|at| {
            at != BackupRef::from_record(op, ordinal, StepDirection::Forward, BackupSlot::Original)
        }) {
            return Err(DeployError::Io);
        }
        let step_pre_backup = step_pre_backup.as_deref().map(decode_backup).transpose()?;
        if step_pre_backup.is_some_and(|at| {
            at != BackupRef::from_record(op, ordinal, StepDirection::Forward, BackupSlot::Step)
        }) {
            return Err(DeployError::Io);
        }
        let staged = match (stage_ref.as_deref(), stage_fingerprint.as_deref()) {
            (Some(at), Some(fingerprint)) => {
                let at = decode_stage(at)?;
                if at != StageRef::from_record(op, ordinal, StepDirection::Forward) {
                    return Err(DeployError::Io);
                }
                Some((at, decode_fingerprint(fingerprint)?))
            }
            (None, None) => None,
            _ => return Err(DeployError::Io),
        };
        let retain = decode_backup(&displaced_ref)?;
        if retain
            != BackupRef::from_record(op, ordinal, StepDirection::Forward, BackupSlot::Displaced)
        {
            return Err(DeployError::Io);
        }
        let undo = undo
            .as_deref()
            .map(decode_prepared)
            .transpose()?
            .map(|undo: PreparedStep| {
                if undo.op != op || undo.ordinal != ordinal || undo.direction != StepDirection::Undo
                {
                    Err(DeployError::Io)
                } else {
                    Ok(undo)
                }
            })
            .transpose()?;

        let prepared = PreparedStep {
            op,
            ordinal,
            direction: StepDirection::Forward,
            spec: StepSpec {
                target: decode_bound(&target)?,
                original: PreImage {
                    observation: decode_pre_state(&original_observation)?,
                    backup: original_backup,
                },
                pre: decode_pre_state(&step_pre_observation)?,
                output: decode_output(&output)?,
            },
            step_pre: PreImage {
                observation: decode_pre_state(&step_pre_observation)?,
                backup: step_pre_backup,
            },
            staged,
            retain,
        };
        steps.push(StepRecord {
            prepared,
            phase: parse_step_phase(&phase)?,
            evidence: observed.as_deref().map(decode_evidence).transpose()?,
            undo,
        });
    }
    Ok(steps)
}

fn read_intents(connection: &Connection, plan: i64) -> Result<Vec<FileIntent>, DeployError> {
    let rows = {
        let mut statement = connection
            .prepare(
                "SELECT role, path, parent, leaf, source_id, source_path, source_sha, \
                 source_len, original FROM plan_intents WHERE plan_id = ?1 ORDER BY ordinal ASC",
            )
            .map_err(|_| DeployError::Io)?;
        let rows = statement
            .query_map([plan], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Vec<u8>>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, Vec<u8>>(8)?,
                ))
            })
            .map_err(|_| DeployError::Io)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|_| DeployError::Io)?
    };

    let mut intents = Vec::with_capacity(rows.len());
    for (role, path, parent, leaf, source_id, source_path, source_sha, source_len, original) in rows
    {
        if role != "data" {
            return Err(DeployError::Io);
        }
        intents.push(FileIntent {
            target: BoundTarget {
                target: TargetFile {
                    root: RootRole::Data,
                    relative: RelativePath::parse(&path).map_err(|_| DeployError::Io)?,
                },
                parent: decode_root(&parent)?,
                leaf,
            },
            source: SourceRef {
                installed: InstalledRevisionId::new(positive(source_id)?),
                path: RelativePath::parse(&source_path).map_err(|_| DeployError::Io)?,
                sha256: sha(&source_sha)?,
                len: nonnegative(source_len)?,
            },
            original: decode_pre_state(&original)?,
        });
    }
    Ok(intents)
}

fn read_plan_paths(connection: &Connection, plan: i64) -> Result<Vec<String>, DeployError> {
    let rows = {
        let mut statement = connection
            .prepare("SELECT path FROM plan_intents WHERE plan_id = ?1 ORDER BY ordinal ASC")
            .map_err(|_| DeployError::Io)?;
        let rows = statement
            .query_map([plan], |row| row.get::<_, String>(0))
            .map_err(|_| DeployError::Io)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|_| DeployError::Io)?
    };
    Ok(rows)
}

fn write_intent(
    connection: &Connection,
    plan: i64,
    ordinal: i64,
    intent: &FileIntent,
) -> Result<(), DeployError> {
    connection
        .execute(
            "INSERT INTO plan_intents \
             (plan_id, ordinal, role, path, parent, leaf, source_id, source_path, \
              source_sha, source_len, original) \
             VALUES (?1, ?2, 'data', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                plan,
                ordinal,
                intent.target.target.relative.as_str(),
                encode_root(&intent.target.parent),
                intent.target.leaf,
                sql_id(intent.source.installed.get())?,
                intent.source.path.as_str(),
                intent.source.sha256.as_slice(),
                sql_nonnegative(intent.source.len)?,
                encode_pre_state(&intent.original)
            ],
        )
        .map_err(|_| DeployError::Io)?;
    Ok(())
}

fn operation_for_plan(
    connection: &Connection,
    plan: i64,
) -> Result<Option<OperationId>, DeployError> {
    let op: Option<i64> = connection
        .query_row(
            "SELECT id FROM operations WHERE plan_id = ?1",
            [plan],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| DeployError::Io)?;
    op.map(row_id)
        .transpose()?
        .map(OperationId::try_from_u64)
        .transpose()
}

fn pending_is(connection: &Connection, data: &[u8], op: OperationId) -> Result<bool, DeployError> {
    let found: Option<i64> = connection
        .query_row(
            "SELECT 1 FROM root_leases WHERE data = ?1 AND pending_op = ?2",
            params![data, sql_id(op.get())?],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| DeployError::Io)?;
    Ok(found.is_some())
}

fn clear_pending(connection: &Connection, data: &[u8], op: OperationId) -> Result<(), DeployError> {
    let changed = connection
        .execute(
            "UPDATE root_leases SET pending_op = NULL \
             WHERE data = ?1 AND pending_op = ?2",
            params![data, sql_id(op.get())?],
        )
        .map_err(|_| DeployError::Io)?;
    if changed != 1 {
        return Err(DeployError::Io);
    }
    Ok(())
}

fn all_steps_applied(connection: &Connection, op: i64) -> Result<bool, DeployError> {
    let (total, applied): (i64, i64) = connection
        .query_row(
            "SELECT count(*), sum(CASE WHEN phase = 'applied' THEN 1 ELSE 0 END) \
             FROM operation_steps WHERE operation_id = ?1",
            [op],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|_| DeployError::Io)?;
    Ok(total > 0 && total == applied)
}

fn valid_pre_image(image: &PreImage) -> bool {
    matches!(
        (&image.observation, &image.backup),
        (PreState::Absent, None) | (PreState::Regular(_), Some(_))
    )
}

fn phase_text(phase: Phase) -> &'static str {
    match phase {
        Phase::Preparing => "preparing",
        Phase::Prepared => "prepared",
        Phase::Applying => "applying",
        Phase::RollingBack => "rolling_back",
        Phase::Restoring => "restoring",
        Phase::Completed => "completed",
        Phase::RolledBack => "rolled_back",
        Phase::Cancelled => "cancelled",
    }
}

fn parse_phase(value: &str) -> Result<Phase, DeployError> {
    match value {
        "preparing" => Ok(Phase::Preparing),
        "prepared" => Ok(Phase::Prepared),
        "applying" => Ok(Phase::Applying),
        "rolling_back" => Ok(Phase::RollingBack),
        "restoring" => Ok(Phase::Restoring),
        "completed" => Ok(Phase::Completed),
        "rolled_back" => Ok(Phase::RolledBack),
        "cancelled" => Ok(Phase::Cancelled),
        _ => Err(DeployError::Io),
    }
}

fn parse_kind(value: &str) -> Result<OperationKind, DeployError> {
    match value {
        "deploy" => Ok(OperationKind::Deploy),
        "restore" => Ok(OperationKind::Restore),
        _ => Err(DeployError::Io),
    }
}

fn step_phase_text(phase: StepPhase) -> &'static str {
    match phase {
        StepPhase::Ready => "ready",
        StepPhase::Attempting => "attempting",
        StepPhase::Applied => "applied",
        StepPhase::UndoAttempting => "undo_attempting",
        StepPhase::Undone => "undone",
    }
}

fn parse_step_phase(value: &str) -> Result<StepPhase, DeployError> {
    match value {
        "ready" => Ok(StepPhase::Ready),
        "attempting" => Ok(StepPhase::Attempting),
        "applied" => Ok(StepPhase::Applied),
        "undo_attempting" => Ok(StepPhase::UndoAttempting),
        "undone" => Ok(StepPhase::Undone),
        _ => Err(DeployError::Io),
    }
}

fn parse_error(value: &str) -> Result<DeployError, DeployError> {
    match value {
        "stale_plan" => Ok(DeployError::StalePlan),
        "active_deployment" => Ok(DeployError::ActiveDeployment),
        "busy" => Ok(DeployError::Busy),
        "target_unavailable" => Ok(DeployError::TargetUnavailable),
        "unsupported_target" => Ok(DeployError::UnsupportedTarget),
        "invalid_path" => Ok(DeployError::InvalidPath),
        "recovery_conflict" => Ok(DeployError::RecoveryConflict),
        "io" => Ok(DeployError::Io),
        "unknown_operation" => Ok(DeployError::UnknownOperation),
        "no_active_deployment" => Ok(DeployError::NoActiveDeployment),
        _ => Err(DeployError::Io),
    }
}

fn sql_id(value: u64) -> Result<i64, DeployError> {
    if value == 0 || value > i64::MAX as u64 {
        return Err(DeployError::Io);
    }
    Ok(value as i64)
}

fn sql_nonnegative(value: u64) -> Result<i64, DeployError> {
    i64::try_from(value).map_err(|_| DeployError::Io)
}

fn row_id(value: i64) -> Result<u64, DeployError> {
    u64::try_from(value).map_err(|_| DeployError::Io)
}

fn positive(value: i64) -> Result<u64, DeployError> {
    let value = u64::try_from(value).map_err(|_| DeployError::Io)?;
    if value == 0 {
        return Err(DeployError::Io);
    }
    Ok(value)
}

fn nonnegative(value: i64) -> Result<u64, DeployError> {
    u64::try_from(value).map_err(|_| DeployError::Io)
}

fn sha(blob: &[u8]) -> Result<[u8; 32], DeployError> {
    blob.try_into().map_err(|_| DeployError::Io)
}

#[cfg(unix)]
fn empty_locator() -> Vec<u8> {
    vec![0]
}

#[cfg(windows)]
fn empty_locator() -> Vec<u8> {
    vec![1]
}

#[cfg(unix)]
fn encode_locator(locator: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    let mut blob = vec![0u8];
    blob.extend_from_slice(locator.as_os_str().as_bytes());
    blob
}

#[cfg(windows)]
fn encode_locator(locator: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    let mut blob = vec![1u8];
    for unit in locator.as_os_str().encode_wide() {
        blob.extend_from_slice(&unit.to_le_bytes());
    }
    blob
}

// Portable evidence encoding v1. Every blob is length-prefixed and rejected on
// a short read, unknown tag or trailing bytes.

fn encode_root(root: &RootIdentity) -> Vec<u8> {
    let mut out = vec![1u8];
    encode_identity(&mut out, root.evidence());
    out
}

fn encode_identity(out: &mut Vec<u8>, identity: &NativeIdentity) {
    match identity {
        NativeIdentity::Linux {
            fs_magic,
            fs_id,
            view_id,
            tree_id,
            ino,
            birth,
        } => {
            out.push(0);
            out.extend_from_slice(&fs_magic.to_le_bytes());
            out.extend_from_slice(fs_id);
            out.extend_from_slice(&view_id.to_le_bytes());
            match tree_id {
                Some(tree_id) => {
                    out.push(1);
                    out.extend_from_slice(&tree_id.to_le_bytes());
                }
                None => out.push(0),
            }
            out.extend_from_slice(&ino.to_le_bytes());
            out.extend_from_slice(&birth.0.to_le_bytes());
            out.extend_from_slice(&birth.1.to_le_bytes());
        }
        NativeIdentity::Windows {
            volume_serial,
            file_id,
            creation_100ns,
        } => {
            out.push(1);
            out.extend_from_slice(&volume_serial.to_le_bytes());
            out.extend_from_slice(file_id);
            out.extend_from_slice(&creation_100ns.to_le_bytes());
        }
    }
}

fn decode_root(blob: &[u8]) -> Result<RootIdentity, DeployError> {
    let mut reader = Reader::new(blob);
    if reader.u8()? != 1 {
        return Err(DeployError::Io);
    }
    let identity = decode_identity(&mut reader)?;
    reader.finish()?;
    Ok(RootIdentity::from_adapter_evidence(identity))
}

fn decode_identity(reader: &mut Reader<'_>) -> Result<NativeIdentity, DeployError> {
    match reader.u8()? {
        0 => Ok(NativeIdentity::Linux {
            fs_magic: reader.u64()?,
            fs_id: reader.array()?,
            view_id: reader.u64()?,
            tree_id: match reader.u8()? {
                0 => None,
                1 => Some(reader.u64()?),
                _ => return Err(DeployError::Io),
            },
            ino: reader.u64()?,
            birth: (reader.i64()?, reader.u32()?),
        }),
        1 => Ok(NativeIdentity::Windows {
            volume_serial: reader.u64()?,
            file_id: reader.array()?,
            creation_100ns: reader.i64()?,
        }),
        _ => Err(DeployError::Io),
    }
}

fn encode_fingerprint(fingerprint: &Fingerprint) -> Vec<u8> {
    let mut out = Vec::new();
    encode_identity(&mut out, &fingerprint.identity);
    out.extend_from_slice(&fingerprint.len.to_le_bytes());
    out.extend_from_slice(&fingerprint.sha256);
    encode_metadata(&mut out, &fingerprint.metadata);
    out
}

fn decode_fingerprint(blob: &[u8]) -> Result<Fingerprint, DeployError> {
    let mut reader = Reader::new(blob);
    let fingerprint = Fingerprint {
        identity: decode_identity(&mut reader)?,
        len: reader.u64()?,
        sha256: reader.array()?,
        metadata: decode_metadata(&mut reader)?,
    };
    reader.finish()?;
    Ok(fingerprint)
}

fn encode_metadata(out: &mut Vec<u8>, metadata: &FileMetadata) {
    match metadata {
        FileMetadata::Linux {
            mode,
            uid,
            gid,
            mtime,
            nlink,
        } => {
            out.push(0);
            out.extend_from_slice(&mode.to_le_bytes());
            out.extend_from_slice(&uid.to_le_bytes());
            out.extend_from_slice(&gid.to_le_bytes());
            out.extend_from_slice(&mtime.0.to_le_bytes());
            out.extend_from_slice(&mtime.1.to_le_bytes());
            out.extend_from_slice(&nlink.to_le_bytes());
        }
        FileMetadata::Windows {
            attributes,
            last_write_100ns,
            links,
            security_sha256,
        } => {
            out.push(1);
            out.extend_from_slice(&attributes.to_le_bytes());
            out.extend_from_slice(&last_write_100ns.to_le_bytes());
            out.extend_from_slice(&links.to_le_bytes());
            out.extend_from_slice(security_sha256);
        }
    }
}

fn decode_metadata(reader: &mut Reader<'_>) -> Result<FileMetadata, DeployError> {
    match reader.u8()? {
        0 => Ok(FileMetadata::Linux {
            mode: reader.u32()?,
            uid: reader.u32()?,
            gid: reader.u32()?,
            mtime: (reader.i64()?, reader.u32()?),
            nlink: reader.u64()?,
        }),
        1 => Ok(FileMetadata::Windows {
            attributes: reader.u32()?,
            last_write_100ns: reader.i64()?,
            links: reader.u32()?,
            security_sha256: reader.array()?,
        }),
        _ => Err(DeployError::Io),
    }
}

fn encode_pre_state(state: &PreState) -> Vec<u8> {
    let mut out = Vec::new();
    match state {
        PreState::Absent => out.push(0),
        PreState::Regular(fingerprint) => {
            out.push(1);
            put_blob(&mut out, &encode_fingerprint(fingerprint));
        }
    }
    out
}

fn decode_pre_state(blob: &[u8]) -> Result<PreState, DeployError> {
    let mut reader = Reader::new(blob);
    let state = match reader.u8()? {
        0 => PreState::Absent,
        1 => PreState::Regular(reader.fingerprint()?),
        _ => return Err(DeployError::Io),
    };
    reader.finish()?;
    Ok(state)
}

fn encode_bound(target: &BoundTarget) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(match target.target.root {
        RootRole::Data => 0,
    });
    put_string(&mut out, target.target.relative.as_str());
    put_blob(&mut out, &encode_root(&target.parent));
    put_string(&mut out, &target.leaf);
    out
}

fn decode_bound(blob: &[u8]) -> Result<BoundTarget, DeployError> {
    let mut reader = Reader::new(blob);
    let root = match reader.u8()? {
        0 => RootRole::Data,
        _ => return Err(DeployError::Io),
    };
    let relative = RelativePath::parse(reader.string()?).map_err(|_| DeployError::Io)?;
    let parent_blob = reader.sized()?;
    let parent = decode_root(parent_blob)?;
    let leaf = reader.string()?.to_owned();
    reader.finish()?;
    Ok(BoundTarget {
        target: TargetFile { root, relative },
        parent,
        leaf,
    })
}

fn encode_output(output: &StepOutput) -> Vec<u8> {
    let mut out = Vec::new();
    match output {
        StepOutput::Source(source) => {
            out.push(0);
            put_blob(&mut out, &encode_source(source));
        }
        StepOutput::Original(at) => {
            out.push(1);
            out.extend_from_slice(&encode_backup(*at));
        }
        StepOutput::Absent => out.push(2),
    }
    out
}

fn decode_output(blob: &[u8]) -> Result<StepOutput, DeployError> {
    let mut reader = Reader::new(blob);
    let output = match reader.u8()? {
        0 => StepOutput::Source(reader.source()?),
        1 => StepOutput::Original(reader.backup()?),
        2 => StepOutput::Absent,
        _ => return Err(DeployError::Io),
    };
    reader.finish()?;
    Ok(output)
}

fn encode_source(source: &SourceRef) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&source.installed.get().to_le_bytes());
    put_string(&mut out, source.path.as_str());
    out.extend_from_slice(&source.sha256);
    out.extend_from_slice(&source.len.to_le_bytes());
    out
}

fn encode_backup(at: BackupRef) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&at.operation().get().to_le_bytes());
    out.extend_from_slice(&at.ordinal().to_le_bytes());
    out.push(match at.direction() {
        StepDirection::Forward => 0,
        StepDirection::Undo => 1,
    });
    out.push(match at.slot() {
        BackupSlot::Original => 0,
        BackupSlot::Step => 1,
        BackupSlot::Displaced => 2,
    });
    out
}

fn decode_backup(blob: &[u8]) -> Result<BackupRef, DeployError> {
    let mut reader = Reader::new(blob);
    let operation = OperationId::try_from_u64(reader.u64()?)?;
    let ordinal = reader.u32()?;
    let direction = match reader.u8()? {
        0 => StepDirection::Forward,
        1 => StepDirection::Undo,
        _ => return Err(DeployError::Io),
    };
    let slot = match reader.u8()? {
        0 => BackupSlot::Original,
        1 => BackupSlot::Step,
        2 => BackupSlot::Displaced,
        _ => return Err(DeployError::Io),
    };
    reader.finish()?;
    Ok(BackupRef::from_record(operation, ordinal, direction, slot))
}

fn encode_stage(at: StageRef) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&at.operation().get().to_le_bytes());
    out.extend_from_slice(&at.ordinal().to_le_bytes());
    out.push(match at.direction() {
        StepDirection::Forward => 0,
        StepDirection::Undo => 1,
    });
    out
}

fn decode_stage(blob: &[u8]) -> Result<StageRef, DeployError> {
    let mut reader = Reader::new(blob);
    let operation = OperationId::try_from_u64(reader.u64()?)?;
    let ordinal = reader.u32()?;
    let direction = match reader.u8()? {
        0 => StepDirection::Forward,
        1 => StepDirection::Undo,
        _ => return Err(DeployError::Io),
    };
    reader.finish()?;
    Ok(StageRef::from_record(operation, ordinal, direction))
}

fn encode_evidence(evidence: &Evidence) -> Vec<u8> {
    let mut out = Vec::new();
    encode_observation(&mut out, &evidence.target);
    encode_artifact(&mut out, &evidence.stage);
    encode_artifact(&mut out, &evidence.retained);
    out
}

fn decode_evidence(blob: &[u8]) -> Result<Evidence, DeployError> {
    let mut reader = Reader::new(blob);
    let evidence = Evidence {
        target: decode_observation(&mut reader)?,
        stage: decode_artifact(&mut reader)?,
        retained: decode_artifact(&mut reader)?,
    };
    reader.finish()?;
    Ok(evidence)
}

fn encode_observation(out: &mut Vec<u8>, observation: &Observation) {
    match observation {
        Observation::Absent => out.push(0),
        Observation::Regular(fingerprint) => {
            out.push(1);
            put_blob(out, &encode_fingerprint(fingerprint));
        }
        Observation::Unavailable(error) => {
            out.push(2);
            encode_root_error(out, error);
        }
    }
}

fn decode_observation(reader: &mut Reader<'_>) -> Result<Observation, DeployError> {
    match reader.u8()? {
        0 => Ok(Observation::Absent),
        1 => Ok(Observation::Regular(reader.fingerprint()?)),
        2 => Ok(Observation::Unavailable(decode_root_error(reader)?)),
        _ => Err(DeployError::Io),
    }
}

fn encode_artifact(out: &mut Vec<u8>, artifact: &Artifact) {
    match artifact {
        Artifact::Absent => out.push(0),
        Artifact::Regular(fingerprint) => {
            out.push(1);
            put_blob(out, &encode_fingerprint(fingerprint));
        }
        Artifact::Unsupported(reason) => {
            out.push(2);
            out.push(reason_tag(*reason));
        }
        Artifact::Unavailable => out.push(3),
    }
}

fn decode_artifact(reader: &mut Reader<'_>) -> Result<Artifact, DeployError> {
    match reader.u8()? {
        0 => Ok(Artifact::Absent),
        1 => Ok(Artifact::Regular(reader.fingerprint()?)),
        2 => Ok(Artifact::Unsupported(decode_reason(reader.u8()?)?)),
        3 => Ok(Artifact::Unavailable),
        _ => Err(DeployError::Io),
    }
}

fn encode_root_error(out: &mut Vec<u8>, error: &RootError) {
    match error {
        RootError::TargetUnavailable => out.push(0),
        RootError::MissingParent => out.push(1),
        RootError::AmbiguousName => out.push(2),
        RootError::OutsideBoundary => out.push(3),
        RootError::UnsupportedTarget(reason) => {
            out.push(4);
            out.push(reason_tag(*reason));
        }
        RootError::Io => out.push(5),
    }
}

fn decode_root_error(reader: &mut Reader<'_>) -> Result<RootError, DeployError> {
    Ok(match reader.u8()? {
        0 => RootError::TargetUnavailable,
        1 => RootError::MissingParent,
        2 => RootError::AmbiguousName,
        3 => RootError::OutsideBoundary,
        4 => RootError::UnsupportedTarget(decode_reason(reader.u8()?)?),
        5 => RootError::Io,
        _ => return Err(DeployError::Io),
    })
}

fn reason_tag(reason: UnsupportedReason) -> u8 {
    match reason {
        UnsupportedReason::ObjectKind => 0,
        UnsupportedReason::Metadata => 1,
        UnsupportedReason::Sharing => 2,
        UnsupportedReason::Configuration => 3,
    }
}

fn decode_reason(value: u8) -> Result<UnsupportedReason, DeployError> {
    match value {
        0 => Ok(UnsupportedReason::ObjectKind),
        1 => Ok(UnsupportedReason::Metadata),
        2 => Ok(UnsupportedReason::Sharing),
        3 => Ok(UnsupportedReason::Configuration),
        _ => Err(DeployError::Io),
    }
}

fn encode_prepared(step: &PreparedStep) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&step.op.get().to_le_bytes());
    out.extend_from_slice(&step.ordinal.to_le_bytes());
    out.push(match step.direction {
        StepDirection::Forward => 0,
        StepDirection::Undo => 1,
    });
    put_blob(&mut out, &encode_bound(&step.spec.target));
    out.extend_from_slice(&encode_pre_image(&step.spec.original));
    put_blob(&mut out, &encode_pre_state(&step.spec.pre));
    put_blob(&mut out, &encode_output(&step.spec.output));
    out.extend_from_slice(&encode_pre_image(&step.step_pre));
    match &step.staged {
        Some((at, fingerprint)) => {
            out.push(1);
            put_blob(&mut out, &encode_stage(*at));
            put_blob(&mut out, &encode_fingerprint(fingerprint));
        }
        None => out.push(0),
    }
    put_blob(&mut out, &encode_backup(step.retain));
    out
}

fn decode_prepared(blob: &[u8]) -> Result<PreparedStep, DeployError> {
    let mut reader = Reader::new(blob);
    let op = OperationId::try_from_u64(reader.u64()?)?;
    let ordinal = reader.u32()?;
    let direction = match reader.u8()? {
        0 => StepDirection::Forward,
        1 => StepDirection::Undo,
        _ => return Err(DeployError::Io),
    };
    let target = decode_bound(reader.sized()?)?;
    let original = decode_pre_image(&mut reader)?;
    let pre = decode_pre_state(reader.sized()?)?;
    let output = decode_output(reader.sized()?)?;
    let step_pre = decode_pre_image(&mut reader)?;
    let staged = match reader.u8()? {
        0 => None,
        1 => Some((decode_stage(reader.sized()?)?, reader.fingerprint()?)),
        _ => return Err(DeployError::Io),
    };
    let retain = decode_backup(reader.sized()?)?;
    reader.finish()?;
    Ok(PreparedStep {
        op,
        ordinal,
        direction,
        spec: StepSpec {
            target,
            original,
            pre,
            output,
        },
        step_pre,
        staged,
        retain,
    })
}

fn encode_pre_image(image: &PreImage) -> Vec<u8> {
    let mut out = Vec::new();
    put_blob(&mut out, &encode_pre_state(&image.observation));
    match image.backup {
        Some(at) => {
            out.push(1);
            put_blob(&mut out, &encode_backup(at));
        }
        None => out.push(0),
    }
    out
}

fn decode_pre_image(reader: &mut Reader<'_>) -> Result<PreImage, DeployError> {
    let state = reader.sized()?;
    let observation = decode_pre_state(state)?;
    let backup = match reader.u8()? {
        0 => None,
        1 => Some(decode_backup(reader.sized()?)?),
        _ => return Err(DeployError::Io),
    };
    if !valid_pre_image(&PreImage {
        observation: observation.clone(),
        backup,
    }) {
        return Err(DeployError::Io);
    }
    Ok(PreImage {
        observation,
        backup,
    })
}

fn put_blob(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u32).to_le_bytes());
    out.extend_from_slice(value);
}

fn put_string(out: &mut Vec<u8>, value: &str) {
    let bytes = value.as_bytes();
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
}

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], DeployError> {
        let end = self.position.checked_add(len).ok_or(DeployError::Io)?;
        let slice = self.bytes.get(self.position..end).ok_or(DeployError::Io)?;
        self.position = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, DeployError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, DeployError> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().map_err(|_| DeployError::Io)?,
        ))
    }

    fn u64(&mut self) -> Result<u64, DeployError> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().map_err(|_| DeployError::Io)?,
        ))
    }

    fn i64(&mut self) -> Result<i64, DeployError> {
        Ok(i64::from_le_bytes(
            self.take(8)?.try_into().map_err(|_| DeployError::Io)?,
        ))
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], DeployError> {
        self.take(N)?.try_into().map_err(|_| DeployError::Io)
    }

    fn string(&mut self) -> Result<&'a str, DeployError> {
        let len = usize::try_from(self.u32()?).map_err(|_| DeployError::Io)?;
        std::str::from_utf8(self.take(len)?).map_err(|_| DeployError::Io)
    }

    fn sized(&mut self) -> Result<&'a [u8], DeployError> {
        let len = usize::try_from(self.u32()?).map_err(|_| DeployError::Io)?;
        self.take(len)
    }

    fn fingerprint(&mut self) -> Result<Fingerprint, DeployError> {
        decode_fingerprint(self.sized()?)
    }

    fn source(&mut self) -> Result<SourceRef, DeployError> {
        let blob = self.sized()?;
        let mut reader = Reader::new(blob);
        let source = SourceRef {
            installed: InstalledRevisionId::new(reader.u64()?),
            path: RelativePath::parse(reader.string()?).map_err(|_| DeployError::Io)?,
            sha256: reader.array()?,
            len: reader.u64()?,
        };
        reader.finish()?;
        Ok(source)
    }

    fn backup(&mut self) -> Result<BackupRef, DeployError> {
        decode_backup(self.sized()?)
    }

    fn finish(&self) -> Result<(), DeployError> {
        if self.position == self.bytes.len() {
            Ok(())
        } else {
            Err(DeployError::Io)
        }
    }
}
