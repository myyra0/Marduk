//! Portable recovery vocabulary and restart reconciliation.
//!
//! This module decides what to do from persisted operation phases and durable
//! observations. Native effects arrive through [`CopyEffects`]; no OS error or
//! path is interpreted here.

use crate::deployment::{
    BoundTarget, DeployError, DeploymentPlan, FileIntent, GenerationId, OperationId, OperationKind,
    OperationStatus, Phase, PlanId, PreState,
};
use crate::ids::{ProfileId, ProfileRevision};
use crate::root_identity::{Fingerprint, RootError, RootIdentity, UnsupportedReason};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum StepDirection {
    Forward,
    Undo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum BackupSlot {
    Original,
    Step,
    Displaced,
    AlternateDisplaced,
}

/// Opaque operation-owned backup reference. It never resolves a caller string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct BackupRef {
    operation: OperationId,
    ordinal: u32,
    direction: StepDirection,
    slot: BackupSlot,
}

impl BackupRef {
    pub fn from_record(
        operation: OperationId,
        ordinal: u32,
        direction: StepDirection,
        slot: BackupSlot,
    ) -> Self {
        Self {
            operation,
            ordinal,
            direction,
            slot,
        }
    }

    pub fn operation(self) -> OperationId {
        self.operation
    }

    pub fn ordinal(self) -> u32 {
        self.ordinal
    }

    pub fn direction(self) -> StepDirection {
        self.direction
    }

    pub fn slot(self) -> BackupSlot {
        self.slot
    }
}

/// Opaque operation-owned staged-output reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct StageRef {
    operation: OperationId,
    ordinal: u32,
    direction: StepDirection,
}

impl StageRef {
    pub fn from_record(operation: OperationId, ordinal: u32, direction: StepDirection) -> Self {
        Self {
            operation,
            ordinal,
            direction,
        }
    }

    pub fn operation(self) -> OperationId {
        self.operation
    }

    pub fn ordinal(self) -> u32 {
        self.ordinal
    }

    pub fn direction(self) -> StepDirection {
        self.direction
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreImage {
    pub observation: PreState,
    pub backup: Option<BackupRef>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StepOutput {
    Source(crate::deployment::SourceRef),
    Original(BackupRef),
    Absent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepSpec {
    pub target: BoundTarget,
    pub original: PreImage,
    pub pre: PreState,
    pub output: StepOutput,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedStep {
    pub op: OperationId,
    pub ordinal: u32,
    pub direction: StepDirection,
    pub spec: StepSpec,
    pub step_pre: PreImage,
    pub staged: Option<(StageRef, Fingerprint)>,
    pub retain: BackupRef,
}

impl PreparedStep {
    /// The deterministic alternate detection slot. It is never a publication
    /// destination; recovery only inspects it so evidence found there can be
    /// reported. Its identity is derived from the sealed step.
    pub fn alternate_displacement(&self) -> BackupRef {
        BackupRef::from_record(
            self.op,
            self.ordinal,
            self.direction,
            BackupSlot::AlternateDisplaced,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepPhase {
    Ready,
    Attempting,
    Applied,
    UndoAttempting,
    Undone,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Observation {
    Absent,
    Regular(Fingerprint),
    Unavailable(RootError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Artifact {
    Absent,
    Regular(Fingerprint),
    Unsupported(UnsupportedReason),
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Evidence {
    pub target: Observation,
    pub stage: Artifact,
    pub retained: Artifact,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EffectOutcome {
    Observed(Evidence),
    Displaced { evidence: Evidence, at: BackupRef },
    Conflict(Evidence),
    Unavailable(RootError),
    Ambiguous(Evidence),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BarrierOutcome {
    Durable(Evidence),
    Unavailable(RootError),
    Ambiguous(Evidence),
}

/// Checked native effect seam. Handles stay in the adapter's lease-scoped
/// session; core supplies only prepared intents and owns no native path.
pub trait CopyEffects {
    fn observe(&mut self, target: &BoundTarget) -> Observation;
    fn prepare(
        &mut self,
        op: OperationId,
        ordinal: u32,
        direction: StepDirection,
        intent: &StepSpec,
    ) -> Result<PreparedStep, DeployError>;
    fn publish_new_absent(&mut self, target: &BoundTarget, staged: StageRef) -> EffectOutcome;
    fn replace_existing_retain_displaced(
        &mut self,
        target: &BoundTarget,
        expected: &Fingerprint,
        staged: StageRef,
        retain: BackupRef,
    ) -> EffectOutcome;
    fn detach_existing_retain(
        &mut self,
        target: &BoundTarget,
        expected: &Fingerprint,
        retain: BackupRef,
    ) -> EffectOutcome;
    fn inspect_step(&mut self, step: &PreparedStep) -> Evidence;
    /// Inspect the second reserved slot. The default is only for ports that
    /// never write to an alternate slot.
    fn inspect_alternate_displacement(&mut self, _step: &PreparedStep) -> Artifact {
        Artifact::Absent
    }
    fn durability_barrier(&mut self, step: &PreparedStep, observed: &Evidence) -> BarrierOutcome;
    /// Verify a durable pre-image without creating a stage or touching a target.
    fn validate_backup(&mut self, at: BackupRef, expected: &Fingerprint)
    -> Result<(), DeployError>;
}

#[derive(Clone, Debug)]
pub struct StepRecord {
    pub prepared: PreparedStep,
    pub phase: StepPhase,
    pub evidence: Option<Evidence>,
    pub undo: Option<PreparedStep>,
}

#[derive(Clone, Debug)]
pub struct GenerationRecord {
    pub id: GenerationId,
    pub deploy: OperationId,
    pub steps: Vec<StepRecord>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RootState {
    pub pending: Option<OperationId>,
    pub active: Option<GenerationId>,
}

#[derive(Clone, Debug)]
pub enum Claim {
    Deploy(DeploymentPlan),
    Restore {
        installation: RootIdentity,
        data: RootIdentity,
        generation: GenerationId,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Finalize {
    DeployCommitted,
    RestoreCompleted,
    RolledBack,
    Cancelled,
}

/// Durable operation ledger. Implementations own SQLite or synthetic storage;
/// core only sees typed rows and compare-and-set outcomes.
pub trait Journal {
    fn reopen(&mut self) -> Result<(), DeployError>;

    fn save_plan(
        &mut self,
        profile: ProfileId,
        revision: ProfileRevision,
        installation: RootIdentity,
        data: RootIdentity,
        intents: Vec<FileIntent>,
    ) -> Result<DeploymentPlan, DeployError>;

    fn plan(&mut self, id: PlanId) -> Result<Option<DeploymentPlan>, DeployError>;

    fn claimed(&mut self, id: PlanId) -> Result<Option<OperationStatus>, DeployError>;

    fn root_state(&mut self, data: &RootIdentity) -> Result<RootState, DeployError>;

    /// Check that the pending operation owns the exact requested root pair.
    /// Durable journals must check their recorded locator binding too.
    fn verify_pending_pair(
        &mut self,
        _op: OperationId,
        _installation: &RootIdentity,
        _data: &RootIdentity,
    ) -> Result<Option<Vec<u8>>, DeployError> {
        Ok(None)
    }

    fn claim(&mut self, claim: Claim) -> Result<OperationId, DeployError>;

    fn seal(&mut self, op: OperationId, steps: Vec<PreparedStep>) -> Result<(), DeployError>;

    fn set_phase(&mut self, op: OperationId, from: Phase, to: Phase) -> Result<(), DeployError>;

    fn record_step(
        &mut self,
        op: OperationId,
        ordinal: u32,
        from: StepPhase,
        to: StepPhase,
        evidence: Option<Evidence>,
    ) -> Result<(), DeployError>;

    fn block(&mut self, op: OperationId, error: Option<DeployError>) -> Result<(), DeployError>;

    /// Keep a known unexpected displacement available for operator recovery.
    /// Durable journals must retain the reported reference and evidence.
    fn block_displacement(
        &mut self,
        op: OperationId,
        _step: &PreparedStep,
        _at: BackupRef,
        _evidence: &Evidence,
    ) -> Result<(), DeployError> {
        self.block(op, Some(DeployError::RecoveryConflict))
    }

    /// Keep an unreadable canonical-slot observation that is not a positive
    /// displacement report. It is evidence, not a blocker by itself.
    fn record_unreadable_retain(
        &mut self,
        _op: OperationId,
        _step: &PreparedStep,
        _at: BackupRef,
        _evidence: &Evidence,
    ) -> Result<(), DeployError> {
        Ok(())
    }

    /// An unexpected displacement requires operator review, not auto-retry.
    fn has_unresolved_displacement(&mut self, _op: OperationId) -> Result<bool, DeployError> {
        Ok(false)
    }

    fn finish(&mut self, op: OperationId, how: Finalize) -> Result<OperationStatus, DeployError>;

    fn status(&mut self, op: OperationId) -> Result<Option<OperationStatus>, DeployError>;

    fn steps(&mut self, op: OperationId) -> Result<Vec<StepRecord>, DeployError>;

    fn generation(&mut self, id: GenerationId) -> Result<GenerationRecord, DeployError>;

    fn record_undo(
        &mut self,
        op: OperationId,
        ordinal: u32,
        undo: PreparedStep,
    ) -> Result<(), DeployError>;
}

/// Resume or roll back the one pending operation for an exact root pair.
///
/// A root that cannot be reopened records `TargetUnavailable` and leaves all
/// evidence in place. A second call after a durable resolution is a no-op.
pub fn recover_pending<J: Journal, P: crate::deployment::DeploymentInputs>(
    journal: &mut J,
    port: &mut P,
    installation: &RootIdentity,
    data: &RootIdentity,
) -> Result<Option<OperationStatus>, DeployError> {
    let _root = port.try_root_gate()?;

    let state = journal.root_state(data)?;
    let Some(op) = state.pending else {
        return Ok(None);
    };

    if let Err(error) = journal
        .verify_pending_pair(op, installation, data)
        .and_then(|locator| port.verify_recovery_root(installation, data, locator.as_deref()))
    {
        journal.block(op, Some(error))?;
        return journal.status(op);
    }

    let status = journal.status(op)?.ok_or(DeployError::UnknownOperation)?;
    // Classify every sealed row before any effect. A failed target read still
    // requires both private slots to be inspected, so hidden evidence cannot
    // be skipped and a later readable row cannot run first.
    let mut found = false;
    let mut unavailable = false;
    let mut scans = Vec::new();
    for row in journal.steps(op)? {
        let forward = port.inspect_step(&row.prepared);
        match preserve_displacements(journal, port, &row.prepared, &forward) {
            Ok(SlotScan::Displaced) => found = true,
            Ok(SlotScan::TargetUnreadable) => {}
            Ok(SlotScan::Clean) => {}
            Err(DeployError::TargetUnavailable) => unavailable = true,
            Err(error) => return blocked(journal, op, error),
        }
        let undo = match &row.undo {
            Some(undo) => {
                let evidence = port.inspect_step(undo);
                match preserve_displacements(journal, port, undo, &evidence) {
                    Ok(SlotScan::Displaced) => found = true,
                    Ok(SlotScan::TargetUnreadable) => {}
                    Ok(SlotScan::Clean) => {}
                    Err(DeployError::TargetUnavailable) => unavailable = true,
                    Err(error) => return blocked(journal, op, error),
                }
                Some(evidence)
            }
            None => None,
        };
        scans.push(ScannedRow { row, forward, undo });
    }
    if found || journal.has_unresolved_displacement(op)? {
        return journal.status(op);
    }
    if unavailable {
        return blocked(journal, op, DeployError::TargetUnavailable);
    }
    match status.kind {
        OperationKind::Deploy => recover_deploy(journal, port, op, status, scans),
        OperationKind::Restore => recover_restore(journal, port, op, status, scans),
    }
}

/// One sealed row plus the evidence the displacement scan already read.
///
/// Recovery classifies every row from this snapshot before it runs any effect,
/// so a conflict cannot be discovered only after an earlier row mutated.
struct ScannedRow {
    row: StepRecord,
    forward: Evidence,
    undo: Option<Evidence>,
}

/// What the two private slots say about a sealed row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlotScan {
    Clean,
    /// A positive artifact was persisted as an operator-visible blocker.
    Displaced,
    /// The public target could not be read and no slot supplied evidence.
    TargetUnreadable,
}

fn preserve_displacements<J: Journal, P: CopyEffects>(
    journal: &mut J,
    port: &mut P,
    step: &PreparedStep,
    evidence: &Evidence,
) -> Result<SlotScan, DeployError> {
    // Both private slots are independent of the public target, so a failed
    // target read must never suppress their inspection.
    let mut found = preserve_canonical_displacement(journal, step, evidence)?;
    match port.inspect_alternate_displacement(step) {
        Artifact::Absent => {}
        Artifact::Unavailable if !found => return Err(DeployError::TargetUnavailable),
        Artifact::Unavailable => {}
        retained => {
            let mut evidence = evidence.clone();
            evidence.retained = retained;
            journal.block_displacement(step.op, step, step.alternate_displacement(), &evidence)?;
            found = true;
        }
    }
    Ok(if found {
        SlotScan::Displaced
    } else if matches!(evidence.target, Observation::Unavailable(_)) {
        SlotScan::TargetUnreadable
    } else {
        SlotScan::Clean
    })
}

pub(crate) fn preserve_canonical_displacement<J: Journal>(
    journal: &mut J,
    step: &PreparedStep,
    evidence: &Evidence,
) -> Result<bool, DeployError> {
    // An unreadable canonical slot proves nothing about its contents. Without
    // a readable target it is not a displacement report, but the observation
    // is still kept so repeated recovery cannot silently lose it.
    if evidence.retained == Artifact::Unavailable {
        if matches!(evidence.target, Observation::Unavailable(_)) {
            journal.record_unreadable_retain(step.op, step, step.retain, evidence)?;
            return Ok(false);
        }
        journal.block_displacement(step.op, step, step.retain, evidence)?;
        return Ok(true);
    }
    // A positively observed unexpected artifact is always a displacement,
    // even when the public target itself could not be read.
    if matches!(
        evidence.retained,
        Artifact::Regular(_) | Artifact::Unsupported(_)
    ) && evidence.retained != crate::deployment::expected_retained(step)
    {
        journal.block_displacement(step.op, step, step.retain, evidence)?;
        return Ok(true);
    }
    Ok(false)
}

pub(crate) fn check_displacements<J: Journal, P: CopyEffects>(
    journal: &mut J,
    port: &mut P,
    step: &PreparedStep,
    evidence: &Evidence,
) -> Result<(), DeployError> {
    match preserve_displacements(journal, port, step, evidence) {
        Ok(SlotScan::Displaced) => Err(DeployError::RecoveryConflict),
        Ok(SlotScan::Clean | SlotScan::TargetUnreadable) => Ok(()),
        Err(DeployError::TargetUnavailable) => {
            journal.block(step.op, Some(DeployError::TargetUnavailable))?;
            Err(DeployError::TargetUnavailable)
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn inspect_checked<J: Journal, P: CopyEffects>(
    journal: &mut J,
    port: &mut P,
    step: &PreparedStep,
) -> Result<Evidence, DeployError> {
    let evidence = port.inspect_step(step);
    check_displacements(journal, port, step, &evidence)?;
    Ok(evidence)
}

fn recover_deploy<J: Journal, P: CopyEffects>(
    journal: &mut J,
    port: &mut P,
    op: OperationId,
    status: OperationStatus,
    scans: Vec<ScannedRow>,
) -> Result<Option<OperationStatus>, DeployError> {
    let rows = journal.steps(op)?;
    match status.phase {
        Phase::Preparing | Phase::Prepared => {
            for row in &rows {
                let evidence = port.inspect_step(&row.prepared);
                if let Err(error) = check_pre(port, &row.prepared, &evidence) {
                    return blocked(journal, op, error);
                }
            }
            return crate::deployment::finish(journal, op, Finalize::Cancelled).map(Some);
        }
        Phase::Applying => journal.set_phase(op, Phase::Applying, Phase::RollingBack)?,
        Phase::RollingBack => {}
        _ => return Ok(Some(status)),
    }
    // Every row's complete rollback state is classified before the first
    // effect, so a later conflict cannot follow an earlier mutation.
    if let Err(error) = classify_deploy(port, &scans) {
        return blocked(journal, op, error);
    }

    for scan in scans.iter().rev() {
        let row = &scan.row;
        let forward = &row.prepared;
        let result = if let Some(undo) = &row.undo {
            resume_undo(journal, port, row, undo)
        } else {
            let evidence = match inspect_checked(journal, port, forward) {
                Ok(evidence) => evidence,
                Err(error) => return blocked(journal, op, error),
            };
            if evidence.target == as_observation(&forward.spec.pre) {
                let result = check_pre(port, forward, &evidence);
                result.and_then(|()| {
                    if row.phase == StepPhase::Undone {
                        return Ok(());
                    }
                    if !matches!(row.phase, StepPhase::Ready | StepPhase::Attempting) {
                        return Err(DeployError::RecoveryConflict);
                    }
                    let durable = barrier(journal, port, forward, &evidence)?;
                    check_pre(port, forward, &durable)?;
                    journal.record_step(
                        op,
                        forward.ordinal,
                        row.phase,
                        StepPhase::Undone,
                        Some(durable),
                    )
                })
            } else {
                (|| {
                    if !matches!(row.phase, StepPhase::Attempting | StepPhase::Applied) {
                        return Err(observation_error(&evidence.target));
                    }
                    check_post(port, forward, &evidence)?;
                    let undo = prepare_undo(
                        port,
                        forward,
                        &crate::deployment::expected_post_state(forward),
                    )?;
                    journal.record_undo(op, forward.ordinal, undo.clone())?;
                    resume_undo(journal, port, row, &undo)
                })()
            }
        };
        if let Err(error) = result {
            return blocked(journal, op, error);
        }
    }

    // Recheck earlier resolutions after later effects, before releasing the lease.
    for row in journal.steps(op)? {
        let result = match &row.undo {
            Some(undo) => check_forward_slots(journal, port, &row.prepared).and_then(|()| {
                let evidence = inspect_checked(journal, port, undo)?;
                check_post(port, undo, &evidence)
            }),
            None => inspect_checked(journal, port, &row.prepared)
                .and_then(|evidence| check_pre(port, &row.prepared, &evidence)),
        };
        if let Err(error) = result {
            return blocked(journal, op, error);
        }
    }
    if status.blocker.is_some() {
        journal.block(op, None)?;
    }
    crate::deployment::finish(journal, op, Finalize::RolledBack).map(Some)
}

fn resume_undo<J: Journal, P: CopyEffects>(
    journal: &mut J,
    port: &mut P,
    row: &StepRecord,
    undo: &PreparedStep,
) -> Result<(), DeployError> {
    let forward = &row.prepared;
    check_forward_slots(journal, port, forward)?;
    let evidence = inspect_checked(journal, port, undo)?;
    if matches!(row.phase, StepPhase::UndoAttempting | StepPhase::Undone)
        && evidence.target == crate::deployment::expected_post(undo)
    {
        check_post(port, undo, &evidence)?;
        if row.phase == StepPhase::Undone {
            return Ok(());
        }
        let durable = barrier(journal, port, undo, &evidence)?;
        check_post(port, undo, &durable)?;
        return journal.record_step(
            undo.op,
            undo.ordinal,
            StepPhase::UndoAttempting,
            StepPhase::Undone,
            Some(durable),
        );
    }
    if !matches!(
        row.phase,
        StepPhase::Attempting | StepPhase::Applied | StepPhase::UndoAttempting
    ) {
        return Err(observation_error(&evidence.target));
    }
    check_pre(port, undo, &evidence)?;
    if row.phase != StepPhase::UndoAttempting {
        journal.record_step(
            undo.op,
            undo.ordinal,
            row.phase,
            StepPhase::UndoAttempting,
            None,
        )?;
    }
    let outcome = crate::deployment::execute_undo(port, undo)?;
    let evidence = crate::deployment::effect_evidence(journal, port, undo.op, undo, outcome)?;
    check_post(port, undo, &evidence)?;
    let durable = barrier(journal, port, undo, &evidence)?;
    check_post(port, undo, &durable)?;
    journal.record_step(
        undo.op,
        undo.ordinal,
        StepPhase::UndoAttempting,
        StepPhase::Undone,
        Some(durable),
    )
}

fn recover_restore<J: Journal, P: CopyEffects>(
    journal: &mut J,
    port: &mut P,
    op: OperationId,
    status: OperationStatus,
    scans: Vec<ScannedRow>,
) -> Result<Option<OperationStatus>, DeployError> {
    let rows = journal.steps(op)?;
    match status.phase {
        Phase::Preparing | Phase::Prepared => {
            for row in &rows {
                let evidence = port.inspect_step(&row.prepared);
                if let Err(error) = check_pre(port, &row.prepared, &evidence) {
                    return blocked(journal, op, error);
                }
            }
            return crate::deployment::finish(journal, op, Finalize::Cancelled).map(Some);
        }
        Phase::Restoring => {}
        _ => return Ok(Some(status)),
    }
    // A row that cannot continue is typed before the first reverse effect, so
    // a readable earlier row cannot mutate ahead of a later conflict.
    if let Err(error) = classify_restore(port, &scans) {
        return blocked(journal, op, error);
    }

    for scan in scans.iter().rev() {
        let step = &scan.row.prepared;
        let row = &scan.row;
        let result = (|| {
            let evidence = inspect_checked(journal, port, step)?;
            let from = row.phase;
            if !matches!(
                from,
                StepPhase::Ready | StepPhase::Attempting | StepPhase::Applied
            ) {
                return Err(DeployError::RecoveryConflict);
            }
            if evidence.target == crate::deployment::expected_post(step) {
                if from == StepPhase::Ready {
                    return Err(DeployError::RecoveryConflict);
                }
                check_post(port, step, &evidence)?;
                let durable = barrier(journal, port, step, &evidence)?;
                check_post(port, step, &durable)?;
                if from == StepPhase::Attempting {
                    journal.record_step(
                        op,
                        step.ordinal,
                        from,
                        StepPhase::Applied,
                        Some(durable),
                    )?;
                }
            } else {
                check_pre(port, step, &evidence)?;
                if from != StepPhase::Attempting {
                    journal.record_step(
                        op,
                        step.ordinal,
                        from,
                        StepPhase::Attempting,
                        (from == StepPhase::Applied).then_some(evidence),
                    )?;
                }
                let outcome = crate::deployment::execute(port, step)?;
                crate::deployment::settle_forward(journal, port, op, step, outcome)?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            return blocked(journal, op, error);
        }
    }

    for row in &rows {
        if let Err(error) = inspect_checked(journal, port, &row.prepared)
            .and_then(|evidence| check_post(port, &row.prepared, &evidence))
        {
            return blocked(journal, op, error);
        }
    }
    if status.blocker.is_some() {
        journal.block(op, None)?;
    }
    crate::deployment::finish(journal, op, Finalize::RestoreCompleted).map(Some)
}

/// Read-only, phase-aware rollback check for every sealed row.
///
/// It mirrors the effect loop's state machine but only observes: target pre,
/// post or unknown, canonical retain, stage evidence and the required original
/// and step pre-image backups. Any row that could not continue is a typed
/// blocker, so no effect runs after a conflict was already discoverable.
fn classify_deploy<P: CopyEffects>(port: &mut P, scans: &[ScannedRow]) -> Result<(), DeployError> {
    for scan in scans {
        let row = &scan.row;
        let forward = &row.prepared;
        let evidence = &scan.forward;
        if matches!(evidence.target, Observation::Unavailable(_)) {
            return Err(observation_error(&evidence.target));
        }
        if let Some(undo) = &row.undo {
            let evidence = scan.undo.as_ref().ok_or(DeployError::RecoveryConflict)?;
            classify_forward_state(port, forward, &scan.forward)?;
            classify_undo(port, row.phase, undo, evidence)?;
            continue;
        }
        if evidence.target == as_observation(&forward.spec.pre) {
            if !matches!(
                row.phase,
                StepPhase::Ready | StepPhase::Attempting | StepPhase::Undone
            ) {
                return Err(DeployError::RecoveryConflict);
            }
            check_pre(port, forward, evidence)?;
        } else {
            if !matches!(row.phase, StepPhase::Attempting | StepPhase::Applied) {
                return Err(observation_error(&evidence.target));
            }
            check_post(port, forward, evidence)?;
            // A rollback of a published forward step needs a preparable undo.
            if !matches!(
                crate::deployment::expected_post_state(forward),
                PreState::Regular(_)
            ) {
                return Err(DeployError::RecoveryConflict);
            }
            if matches!(forward.spec.pre, PreState::Regular(_))
                && (!matches!(forward.step_pre.observation, PreState::Regular(_))
                    || forward.step_pre.backup.is_none())
            {
                return Err(DeployError::RecoveryConflict);
            }
        }
    }
    Ok(())
}

/// The undo half of the same read-only classification.
fn classify_undo<P: CopyEffects>(
    port: &mut P,
    phase: StepPhase,
    undo: &PreparedStep,
    evidence: &Evidence,
) -> Result<(), DeployError> {
    if matches!(evidence.target, Observation::Unavailable(_)) {
        return Err(observation_error(&evidence.target));
    }
    let settled = matches!(phase, StepPhase::UndoAttempting | StepPhase::Undone)
        && evidence.target == crate::deployment::expected_post(undo);
    if settled {
        return check_post(port, undo, evidence);
    }
    if !matches!(
        phase,
        StepPhase::Attempting | StepPhase::Applied | StepPhase::UndoAttempting
    ) {
        return Err(observation_error(&evidence.target));
    }
    check_pre(port, undo, evidence)
}

/// Read-only, phase-aware continuation check for every Restore row.
fn classify_restore<P: CopyEffects>(port: &mut P, scans: &[ScannedRow]) -> Result<(), DeployError> {
    for scan in scans {
        let row = &scan.row;
        let step = &row.prepared;
        let evidence = &scan.forward;
        if matches!(evidence.target, Observation::Unavailable(_)) {
            return Err(observation_error(&evidence.target));
        }
        if !matches!(
            row.phase,
            StepPhase::Ready | StepPhase::Attempting | StepPhase::Applied
        ) {
            return Err(DeployError::RecoveryConflict);
        }
        if evidence.target == crate::deployment::expected_post(step) {
            if row.phase == StepPhase::Ready {
                return Err(DeployError::RecoveryConflict);
            }
            check_post(port, step, evidence)?;
        } else {
            check_pre(port, step, evidence)?;
        }
    }
    Ok(())
}

fn blocked<J: Journal>(
    journal: &mut J,
    op: OperationId,
    error: DeployError,
) -> Result<Option<OperationStatus>, DeployError> {
    journal.block(op, Some(error))?;
    if error == DeployError::Io {
        return Err(error);
    }
    journal.status(op)
}

fn observation_error(observation: &Observation) -> DeployError {
    match observation {
        Observation::Unavailable(error) => crate::deployment::map_root_error(error.clone()),
        _ => DeployError::RecoveryConflict,
    }
}

/// The typed blocker for a target that could not be read, if any.
///
/// A failed target read proves nothing about the namespace, so an effect must
/// not run on it. Callers that are about to execute persist this before
/// returning it.
pub(crate) fn target_error(evidence: &Evidence) -> Option<DeployError> {
    match &evidence.target {
        Observation::Unavailable(error) => Some(crate::deployment::map_root_error(error.clone())),
        _ => None,
    }
}

fn check_backup<P: CopyEffects>(port: &mut P, image: &PreImage) -> Result<(), DeployError> {
    match (&image.observation, image.backup) {
        (PreState::Absent, None) => Ok(()),
        (PreState::Regular(expected), Some(at)) => port.validate_backup(at, expected),
        _ => Err(DeployError::RecoveryConflict),
    }
}

pub(crate) fn check_pre<P: CopyEffects>(
    port: &mut P,
    step: &PreparedStep,
    evidence: &Evidence,
) -> Result<(), DeployError> {
    if !matches_pre(step, evidence) {
        return Err(observation_error(&evidence.target));
    }
    check_backup(port, &step.spec.original)?;
    check_backup(port, &step.step_pre)
}

/// Compare recorded evidence against the complete pre-effect namespace.
pub fn matches_pre(step: &PreparedStep, evidence: &Evidence) -> bool {
    matches_step_state(
        &step.spec.pre,
        step.staged.as_ref().map(|(_, fp)| fp),
        false,
        evidence,
    )
}

/// Compare recorded evidence against the complete post-effect namespace.
pub fn matches_post(step: &PreparedStep, evidence: &Evidence) -> bool {
    matches_step_state(
        &step.spec.pre,
        step.staged.as_ref().map(|(_, fp)| fp),
        true,
        evidence,
    )
}

/// Check phase evidence against the recorded pre-image and staged token.
pub fn matches_step_state(
    pre: &PreState,
    staged: Option<&Fingerprint>,
    post: bool,
    evidence: &Evidence,
) -> bool {
    if post {
        evidence.target == staged.map_or(Observation::Absent, |fp| Observation::Regular(fp.clone()))
            && evidence.stage == Artifact::Absent
            && evidence.retained
                == match pre {
                    PreState::Absent => Artifact::Absent,
                    PreState::Regular(fp) => Artifact::Regular(fp.clone()),
                }
    } else {
        evidence.target == as_observation(pre)
            && evidence.stage == staged.map_or(Artifact::Absent, |fp| Artifact::Regular(fp.clone()))
            && evidence.retained == Artifact::Absent
    }
}

pub(crate) fn check_post<P: CopyEffects>(
    port: &mut P,
    step: &PreparedStep,
    evidence: &Evidence,
) -> Result<(), DeployError> {
    if !matches_post(step, evidence) {
        return Err(observation_error(&evidence.target));
    }
    check_backup(port, &step.spec.original)?;
    check_backup(port, &step.step_pre)
}

fn check_forward_slots<J: Journal, P: CopyEffects>(
    journal: &mut J,
    port: &mut P,
    forward: &PreparedStep,
) -> Result<(), DeployError> {
    let evidence = inspect_checked(journal, port, forward)?;
    classify_forward_state(port, forward, &evidence)
}

fn classify_forward_state<P: CopyEffects>(
    port: &mut P,
    forward: &PreparedStep,
    evidence: &Evidence,
) -> Result<(), DeployError> {
    if matches!(evidence.target, Observation::Unavailable(_))
        || evidence.stage != Artifact::Absent
        || evidence.retained != crate::deployment::expected_retained(forward)
    {
        return Err(observation_error(&evidence.target));
    }
    check_backup(port, &forward.spec.original)?;
    check_backup(port, &forward.step_pre)
}

fn barrier<J: Journal, P: CopyEffects>(
    journal: &mut J,
    port: &mut P,
    step: &PreparedStep,
    evidence: &Evidence,
) -> Result<Evidence, DeployError> {
    match port.durability_barrier(step, evidence) {
        BarrierOutcome::Durable(evidence) => {
            check_displacements(journal, port, step, &evidence)?;
            Ok(evidence)
        }
        BarrierOutcome::Unavailable(error) => Err(crate::deployment::map_root_error(error)),
        BarrierOutcome::Ambiguous(evidence) => {
            check_displacements(journal, port, step, &evidence)?;
            Err(DeployError::Io)
        }
    }
}

fn as_observation(state: &PreState) -> Observation {
    match state {
        PreState::Absent => Observation::Absent,
        PreState::Regular(fingerprint) => Observation::Regular(fingerprint.clone()),
    }
}

fn prepare_undo<P: CopyEffects>(
    port: &mut P,
    forward: &PreparedStep,
    observed: &PreState,
) -> Result<PreparedStep, DeployError> {
    let PreState::Regular(_) = observed else {
        return Err(DeployError::RecoveryConflict);
    };
    let output = match &forward.spec.pre {
        PreState::Absent => StepOutput::Absent,
        PreState::Regular(_) => {
            let at = forward
                .step_pre
                .backup
                .ok_or(DeployError::RecoveryConflict)?;
            let PreState::Regular(expected) = &forward.step_pre.observation else {
                return Err(DeployError::RecoveryConflict);
            };
            port.validate_backup(at, expected)?;
            StepOutput::Original(at)
        }
    };
    port.prepare(
        forward.op,
        forward.ordinal,
        StepDirection::Undo,
        &StepSpec {
            target: forward.spec.target.clone(),
            original: forward.spec.original.clone(),
            pre: observed.clone(),
            output,
        },
    )
}
