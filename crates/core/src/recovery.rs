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

    if let Err(error) = port.verify_root(installation, data) {
        journal.block(op, Some(error))?;
        return journal.status(op);
    }

    let status = journal.status(op)?.ok_or(DeployError::UnknownOperation)?;
    match status.kind {
        OperationKind::Deploy => recover_deploy(journal, port, op, status),
        OperationKind::Restore => recover_restore(journal, port, op, status),
    }
}

fn recover_deploy<J: Journal, P: CopyEffects>(
    journal: &mut J,
    port: &mut P,
    op: OperationId,
    status: OperationStatus,
) -> Result<Option<OperationStatus>, DeployError> {
    let rows = journal.steps(op)?;
    if rows.is_empty() {
        return match status.phase {
            Phase::Preparing | Phase::Prepared => Ok(Some(crate::deployment::finish(
                journal,
                op,
                Finalize::Cancelled,
            )?)),
            Phase::Applying => {
                journal.set_phase(op, Phase::Applying, Phase::RollingBack)?;
                Ok(Some(crate::deployment::finish(
                    journal,
                    op,
                    Finalize::RolledBack,
                )?))
            }
            Phase::RollingBack => Ok(Some(crate::deployment::finish(
                journal,
                op,
                Finalize::RolledBack,
            )?)),
            _ => Ok(Some(status)),
        };
    }

    match status.phase {
        Phase::Prepared => {
            // No public phase was opened. Only cancel after confirming every
            // destination still matches the recorded pre-state.
            for row in rows.iter().rev() {
                let observed = match observe(port, &row.prepared.spec.target) {
                    Ok(observed) => observed,
                    Err(error) => {
                        journal.block(op, Some(error))?;
                        return journal.status(op);
                    }
                };
                if observed != row.prepared.spec.pre {
                    journal.block(op, Some(DeployError::RecoveryConflict))?;
                    return journal.status(op);
                }
            }
            return Ok(Some(crate::deployment::finish(
                journal,
                op,
                Finalize::Cancelled,
            )?));
        }
        Phase::Applying => journal.set_phase(op, Phase::Applying, Phase::RollingBack)?,
        Phase::RollingBack => {}
        phase if phase.is_terminal() => return Ok(Some(status)),
        _ => return Ok(Some(status)),
    }

    for row in rows.iter().rev() {
        if row.phase == StepPhase::Undone {
            continue;
        }
        let forward = &row.prepared;
        let observed = match observe(port, &forward.spec.target) {
            Ok(observed) => observed,
            Err(error) => {
                journal.block(op, Some(error))?;
                return journal.status(op);
            }
        };

        if row.phase == StepPhase::UndoAttempting {
            let undo = row.undo.as_ref().ok_or(DeployError::RecoveryConflict)?;
            if observed == forward.spec.pre {
                // The undo result is visible; only barrier evidence is missing.
                let evidence = port.inspect_step(undo);
                if let Err(error) = crate::deployment::validate_settled(undo, &evidence) {
                    journal.block(op, Some(error))?;
                    return journal.status(op);
                }
                match port.durability_barrier(undo, &evidence) {
                    BarrierOutcome::Durable(evidence) => {
                        if let Err(error) = crate::deployment::validate_settled(undo, &evidence) {
                            journal.block(op, Some(error))?;
                            return journal.status(op);
                        }
                        journal.record_step(
                            op,
                            forward.ordinal,
                            StepPhase::UndoAttempting,
                            StepPhase::Undone,
                            Some(evidence),
                        )?;
                    }
                    BarrierOutcome::Ambiguous(_) => {
                        journal.block(op, Some(DeployError::Io))?;
                        return Err(DeployError::Io);
                    }
                    BarrierOutcome::Unavailable(error) => {
                        let error = crate::deployment::map_root_error(error);
                        journal.block(op, Some(error))?;
                        return journal.status(op);
                    }
                }
                continue;
            }
            if observed == crate::deployment::expected_post_state(forward) {
                let outcome = crate::deployment::execute_undo(port, undo)?;
                match settle_undo(journal, port, op, forward.ordinal, undo, outcome) {
                    Ok(()) => continue,
                    Err(DeployError::Io) => return Err(DeployError::Io),
                    Err(_) => return journal.status(op),
                }
            }
            journal.block(op, Some(DeployError::RecoveryConflict))?;
            return journal.status(op);
        }

        if observed == forward.spec.pre {
            // Matching pre-state with intact expected slots: no undo.
            continue;
        }
        if observed != crate::deployment::expected_post_state(forward) {
            // Unknown bytes, unknown identity, or an unavailable object.
            journal.block(op, Some(DeployError::RecoveryConflict))?;
            return journal.status(op);
        }

        // Reuse a recorded undo intent even if the phase write was lost.
        let undo = match &row.undo {
            Some(undo) => undo.clone(),
            None => {
                let undo = match prepare_undo(port, forward, &observed) {
                    Ok(undo) => undo,
                    Err(error) => {
                        journal.block(op, Some(error))?;
                        return journal.status(op);
                    }
                };
                journal.record_undo(op, forward.ordinal, undo.clone())?;
                undo
            }
        };
        journal.record_step(
            op,
            forward.ordinal,
            row.phase,
            StepPhase::UndoAttempting,
            None,
        )?;
        let outcome = crate::deployment::execute_undo(port, &undo)?;
        match settle_undo(journal, port, op, forward.ordinal, &undo, outcome) {
            Ok(()) => {}
            Err(DeployError::Io) => return Err(DeployError::Io),
            Err(_) => return journal.status(op),
        }
    }

    if status.blocker.is_some() {
        journal.block(op, None)?;
    }
    Ok(Some(crate::deployment::finish(
        journal,
        op,
        Finalize::RolledBack,
    )?))
}

fn recover_restore<J: Journal, P: CopyEffects>(
    journal: &mut J,
    port: &mut P,
    op: OperationId,
    status: OperationStatus,
) -> Result<Option<OperationStatus>, DeployError> {
    let rows = journal.steps(op)?;
    match status.phase {
        Phase::Preparing | Phase::Prepared => {
            // No public restore effect yet: a terminal cancellation leaves the
            // active generation untouched.
            return Ok(Some(crate::deployment::finish(
                journal,
                op,
                Finalize::Cancelled,
            )?));
        }
        Phase::Restoring => {}
        phase if phase.is_terminal() => return Ok(Some(status)),
        _ => return Ok(Some(status)),
    }
    if rows.is_empty() {
        return match status.phase {
            Phase::Preparing | Phase::Prepared => Ok(Some(crate::deployment::finish(
                journal,
                op,
                Finalize::Cancelled,
            )?)),
            Phase::Restoring => {
                if status.blocker.is_some() {
                    journal.block(op, None)?;
                }
                Ok(Some(crate::deployment::finish(
                    journal,
                    op,
                    Finalize::RestoreCompleted,
                )?))
            }
            _ => Ok(Some(status)),
        };
    }

    for row in rows.iter().rev() {
        let step = &row.prepared;
        let observed = match observe(port, &step.spec.target) {
            Ok(observed) => observed,
            Err(error) => {
                journal.block(op, Some(error))?;
                return journal.status(op);
            }
        };
        match row.phase {
            StepPhase::Applied => {
                if observed == crate::deployment::expected_post_state(step) {
                    // Already visible; require barrier evidence without
                    // claiming a second effect.
                    let evidence = port.inspect_step(step);
                    if let Err(error) = crate::deployment::validate_settled(step, &evidence) {
                        journal.block(op, Some(error))?;
                        return journal.status(op);
                    }
                    match port.durability_barrier(step, &evidence) {
                        BarrierOutcome::Durable(evidence) => {
                            if let Err(error) = crate::deployment::validate_settled(step, &evidence)
                            {
                                journal.block(op, Some(error))?;
                                return journal.status(op);
                            }
                        }
                        BarrierOutcome::Ambiguous(_) => {
                            journal.block(op, Some(DeployError::Io))?;
                            return Err(DeployError::Io);
                        }
                        BarrierOutcome::Unavailable(error) => {
                            let error = crate::deployment::map_root_error(error);
                            journal.block(op, Some(error))?;
                            return journal.status(op);
                        }
                    }
                } else if observed == step.spec.pre {
                    // The recorded result is not visible. Classify from the
                    // adapter's observation, then re-execute from the sealed
                    // intent and settle from the recorded phase.
                    let evidence = port.inspect_step(step);
                    if evidence.target != as_observation(&step.spec.pre) {
                        journal.block(op, Some(DeployError::RecoveryConflict))?;
                        return journal.status(op);
                    }
                    let outcome = crate::deployment::execute(port, step)?;
                    match crate::deployment::settle_forward_from(
                        journal,
                        port,
                        op,
                        step,
                        StepPhase::Applied,
                        outcome,
                    ) {
                        Ok(()) => {}
                        Err(DeployError::Io) => return Err(DeployError::Io),
                        Err(_) => return journal.status(op),
                    }
                } else {
                    journal.block(op, Some(DeployError::RecoveryConflict))?;
                    return journal.status(op);
                }
            }
            StepPhase::Ready | StepPhase::Attempting => {
                if observed == crate::deployment::expected_post_state(step) {
                    // The public effect is visible but its SQL acknowledgement
                    // was lost. Acknowledge it with fresh barrier evidence.
                    let evidence = port.inspect_step(step);
                    if let Err(error) = crate::deployment::validate_settled(step, &evidence) {
                        journal.block(op, Some(error))?;
                        return journal.status(op);
                    }
                    match port.durability_barrier(step, &evidence) {
                        BarrierOutcome::Durable(evidence) => {
                            if let Err(error) = crate::deployment::validate_settled(step, &evidence)
                            {
                                journal.block(op, Some(error))?;
                                return journal.status(op);
                            }
                            journal.record_step(
                                op,
                                step.ordinal,
                                row.phase,
                                StepPhase::Applied,
                                Some(evidence),
                            )?;
                        }
                        BarrierOutcome::Ambiguous(_) => {
                            journal.block(op, Some(DeployError::Io))?;
                            return Err(DeployError::Io);
                        }
                        BarrierOutcome::Unavailable(error) => {
                            let error = crate::deployment::map_root_error(error);
                            journal.block(op, Some(error))?;
                            return journal.status(op);
                        }
                    }
                } else if observed == step.spec.pre {
                    let evidence = port.inspect_step(step);
                    if evidence.target != as_observation(&step.spec.pre) {
                        journal.block(op, Some(DeployError::RecoveryConflict))?;
                        return journal.status(op);
                    }
                    let from = if row.phase == StepPhase::Ready {
                        journal.record_step(
                            op,
                            step.ordinal,
                            StepPhase::Ready,
                            StepPhase::Attempting,
                            None,
                        )?;
                        StepPhase::Attempting
                    } else {
                        row.phase
                    };
                    let outcome = crate::deployment::execute(port, step)?;
                    match crate::deployment::settle_forward_from(
                        journal, port, op, step, from, outcome,
                    ) {
                        Ok(()) => {}
                        Err(DeployError::Io) => return Err(DeployError::Io),
                        Err(_) => return journal.status(op),
                    }
                } else {
                    journal.block(op, Some(DeployError::RecoveryConflict))?;
                    return journal.status(op);
                }
            }
            StepPhase::UndoAttempting | StepPhase::Undone => {
                journal.block(op, Some(DeployError::RecoveryConflict))?;
                return journal.status(op);
            }
        }
    }

    if status.blocker.is_some() {
        journal.block(op, None)?;
    }
    Ok(Some(crate::deployment::finish(
        journal,
        op,
        Finalize::RestoreCompleted,
    )?))
}

fn as_observation(state: &PreState) -> Observation {
    match state {
        PreState::Absent => Observation::Absent,
        PreState::Regular(fingerprint) => Observation::Regular(fingerprint.clone()),
    }
}

fn observe<P: CopyEffects>(port: &mut P, target: &BoundTarget) -> Result<PreState, DeployError> {
    match port.observe(target) {
        Observation::Absent => Ok(PreState::Absent),
        Observation::Regular(fingerprint) => Ok(PreState::Regular(fingerprint)),
        Observation::Unavailable(error) => Err(crate::deployment::map_root_error(error)),
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

fn settle_undo<J: Journal, P: CopyEffects>(
    journal: &mut J,
    port: &mut P,
    op: OperationId,
    ordinal: u32,
    undo: &PreparedStep,
    outcome: EffectOutcome,
) -> Result<(), DeployError> {
    let evidence = match outcome {
        EffectOutcome::Observed(evidence) => {
            if let Err(error) = crate::deployment::validate_settled(undo, &evidence) {
                journal.block(op, Some(error))?;
                return Err(error);
            }
            evidence
        }
        EffectOutcome::Displaced { evidence, at } => {
            if let Err(error) = crate::deployment::validate_settled(undo, &evidence) {
                journal.block(op, Some(error))?;
                return Err(error);
            }
            if at != undo.retain {
                journal.block(op, Some(DeployError::RecoveryConflict))?;
                return Err(DeployError::RecoveryConflict);
            }
            evidence
        }
        EffectOutcome::Conflict(_) => {
            journal.block(op, Some(DeployError::RecoveryConflict))?;
            return Err(DeployError::RecoveryConflict);
        }
        EffectOutcome::Unavailable(error) => {
            let error = crate::deployment::map_root_error(error);
            journal.block(op, Some(error))?;
            return Err(error);
        }
        EffectOutcome::Ambiguous(_) => {
            journal.block(op, Some(DeployError::Io))?;
            return Err(DeployError::Io);
        }
    };
    match port.durability_barrier(undo, &evidence) {
        BarrierOutcome::Durable(evidence) => {
            if let Err(error) = crate::deployment::validate_settled(undo, &evidence) {
                journal.block(op, Some(error))?;
                return Err(error);
            }
            journal.record_step(
                op,
                ordinal,
                StepPhase::UndoAttempting,
                StepPhase::Undone,
                Some(evidence),
            )
        }
        BarrierOutcome::Unavailable(error) => {
            let error = crate::deployment::map_root_error(error);
            journal.block(op, Some(error))?;
            Err(error)
        }
        BarrierOutcome::Ambiguous(_) => {
            journal.block(op, Some(DeployError::Io))?;
            Err(DeployError::Io)
        }
    }
}
