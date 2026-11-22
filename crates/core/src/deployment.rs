//! Deployment planning, commit, restore and status services.
//!
//! The core sees checked intents, ownership tokens and stable outcomes. Every
//! native read, gate and effect is supplied by [`DeploymentInputs`]; this module
//! never names a syscall or a filesystem path.

use crate::ids::{InstalledRevisionId, ProfileId, ProfileRevision};
use crate::profiles::ProfileSnapshot;
use crate::recovery::{
    Artifact, BackupRef, BarrierOutcome, Claim, CopyEffects, EffectOutcome, Evidence, Finalize,
    Journal, Observation, PreImage, PreparedStep, StepDirection, StepOutput, StepPhase, StepSpec,
};
use crate::resolve::{ResolveError, ResolvedInputs, resolve_files};
use crate::root_identity::{Fingerprint, RootError, RootIdentity};
use crate::targets::{RelativePath, TargetFile};

macro_rules! checked_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(u64);

        impl $name {
            /// Only positive signed-64-bit identities are portable.
            pub fn try_from_u64(value: u64) -> Result<Self, DeployError> {
                if value == 0 || value > i64::MAX as u64 {
                    return Err(DeployError::Io);
                }
                Ok(Self(value))
            }

            pub fn get(self) -> u64 {
                self.0
            }
        }
    };
}

checked_id!(PlanId);
checked_id!(OperationId);
checked_id!(GenerationId);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeploymentPlan {
    pub id: PlanId,
    pub profile: ProfileId,
    pub revision: ProfileRevision,
    pub installation: RootIdentity,
    pub data: RootIdentity,
    pub intents: Vec<FileIntent>,
}

#[derive(Clone, Debug)]
pub struct SourceRef {
    pub installed: InstalledRevisionId,
    pub path: RelativePath,
    pub sha256: [u8; 32],
    pub len: u64,
}

impl PartialEq for SourceRef {
    fn eq(&self, other: &Self) -> bool {
        self.installed == other.installed
            && self.path.as_str() == other.path.as_str()
            && self.sha256 == other.sha256
            && self.len == other.len
    }
}

impl Eq for SourceRef {}

/// A plan intent bound to the exact parent identity and leaf spelling observed
/// at preview. Comparison is textual plus the portable parent evidence; it is
/// not a live path.
#[derive(Clone, Debug)]
pub struct BoundTarget {
    pub target: TargetFile,
    pub parent: RootIdentity,
    pub leaf: String,
}

impl PartialEq for BoundTarget {
    fn eq(&self, other: &Self) -> bool {
        self.target.root == other.target.root
            && self.target.relative.as_str() == other.target.relative.as_str()
            && self.parent == other.parent
            && self.leaf == other.leaf
    }
}

impl Eq for BoundTarget {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreState {
    Absent,
    Regular(Fingerprint),
}

#[derive(Clone, Debug)]
pub struct FileIntent {
    pub target: BoundTarget,
    pub source: SourceRef,
    pub original: PreState,
}

impl PartialEq for FileIntent {
    fn eq(&self, other: &Self) -> bool {
        self.target == other.target
            && self.source == other.source
            && self.original == other.original
    }
}

impl Eq for FileIntent {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Preparing,
    Prepared,
    Applying,
    RollingBack,
    Restoring,
    Completed,
    RolledBack,
    Cancelled,
}

impl Phase {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::RolledBack | Self::Cancelled)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeployError {
    StalePlan,
    ActiveDeployment,
    Busy,
    TargetUnavailable,
    UnsupportedTarget,
    InvalidPath,
    RecoveryConflict,
    Io,
    UnknownOperation,
    NoActiveDeployment,
}

impl DeployError {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StalePlan => "stale_plan",
            Self::ActiveDeployment => "active_deployment",
            Self::Busy => "busy",
            Self::TargetUnavailable => "target_unavailable",
            Self::UnsupportedTarget => "unsupported_target",
            Self::InvalidPath => "invalid_path",
            Self::RecoveryConflict => "recovery_conflict",
            Self::Io => "io",
            Self::UnknownOperation => "unknown_operation",
            Self::NoActiveDeployment => "no_active_deployment",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationKind {
    Deploy,
    Restore,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationStatus {
    pub id: OperationId,
    pub kind: OperationKind,
    pub phase: Phase,
    pub generation: Option<GenerationId>,
    pub blocker: Option<DeployError>,
    pub retained: Vec<BackupRef>,
}

/// Trusted adapter seam for one deployment service call.
///
/// Guards own the acquired lease for the whole call. They precede every other
/// method; a recorded claim is served without acquiring anything.
pub trait DeploymentInputs: CopyEffects {
    type RootGuard;
    type LibraryGuard;

    fn try_root_gate(&mut self) -> Result<Self::RootGuard, DeployError>;
    fn try_library_gate(&mut self) -> Result<Self::LibraryGuard, DeployError>;
    fn verify_root(
        &mut self,
        installation: &RootIdentity,
        data: &RootIdentity,
    ) -> Result<(), DeployError>;
    /// Native adapters must reopen the journal's recorded locator and verify
    /// the exact pair. Synthetic ports may use their existing in-memory binding.
    fn verify_recovery_root(
        &mut self,
        installation: &RootIdentity,
        data: &RootIdentity,
        _locator: Option<&[u8]>,
    ) -> Result<(), DeployError> {
        self.verify_root(installation, data)
    }
    fn resolve_target(&mut self, target: &TargetFile) -> Result<BoundTarget, RootError>;
    fn profile_revision(&mut self, profile: ProfileId) -> Result<ProfileRevision, DeployError>;
    fn source(
        &mut self,
        installed: InstalledRevisionId,
        path: &RelativePath,
    ) -> Result<SourceRef, DeployError>;
}

pub(crate) fn map_root_error(error: RootError) -> DeployError {
    match error {
        RootError::TargetUnavailable => DeployError::TargetUnavailable,
        RootError::MissingParent | RootError::AmbiguousName | RootError::OutsideBoundary => {
            DeployError::InvalidPath
        }
        RootError::UnsupportedTarget(_) => DeployError::UnsupportedTarget,
        RootError::Io => DeployError::Io,
    }
}

fn map_resolve_error(error: ResolveError) -> DeployError {
    match error {
        ResolveError::DuplicateMember(_) | ResolveError::MissingManifest(_) => {
            DeployError::StalePlan
        }
        ResolveError::AliasedEntries { .. } | ResolveError::ConflictingTargetShape { .. } => {
            DeployError::InvalidPath
        }
    }
}

/// Validate the exact snapshot, roots, sources and destinations, then persist a
/// plan. No game directory is written and no operation is claimed.
pub fn preview_deployment<J: Journal, P: DeploymentInputs>(
    journal: &mut J,
    port: &mut P,
    profile: &ProfileSnapshot,
    inputs: &ResolvedInputs,
    installation: &RootIdentity,
    data: &RootIdentity,
) -> Result<DeploymentPlan, DeployError> {
    port.verify_root(installation, data)?;
    if port.profile_revision(profile.id)? != profile.revision {
        return Err(DeployError::StalePlan);
    }

    let state = journal.root_state(data)?;
    if state.pending.is_some() {
        return Err(DeployError::Busy);
    }
    if state.active.is_some() {
        return Err(DeployError::ActiveDeployment);
    }

    let resolved = resolve_files(profile, inputs).map_err(map_resolve_error)?;
    if resolved.revision != profile.revision {
        return Err(DeployError::StalePlan);
    }

    let mut intents = Vec::with_capacity(resolved.entries.len());
    for entry in resolved.entries {
        let winner = entry.candidates.last().ok_or(DeployError::StalePlan)?;
        let bound = port.resolve_target(&entry.target).map_err(map_root_error)?;
        // The merged target spelling is authoritative for both the destination
        // and the source lookup; the installed payload may store the same
        // canonical entry under an archive-provided spelling.
        let source = port.source(winner.installed, &entry.target.relative)?;
        let original = match port.observe(&bound) {
            Observation::Absent => PreState::Absent,
            Observation::Regular(fingerprint) => PreState::Regular(fingerprint),
            Observation::Unavailable(error) => return Err(map_root_error(error)),
        };
        intents.push(FileIntent {
            target: bound,
            source,
            original,
        });
    }

    journal.save_plan(
        profile.id,
        profile.revision,
        installation.clone(),
        data.clone(),
        intents,
    )
}

/// Claim and execute a reviewed deployment plan in canonical target order.
///
/// A recorded claim is returned before gates, revisions or effects are touched.
/// An ambiguous SQL acknowledgement is reopened and inspected, never retried as
/// a new effect.
pub fn commit_deployment<J: Journal, P: DeploymentInputs>(
    journal: &mut J,
    port: &mut P,
    id: PlanId,
    mut cancelled: impl FnMut() -> bool,
) -> Result<OperationStatus, DeployError> {
    if let Some(status) = journal.claimed(id)? {
        return Ok(status);
    }

    let plan = journal.plan(id)?.ok_or(DeployError::StalePlan)?;
    let _root = port.try_root_gate()?;

    // Recheck under the exclusive lease; another client may have claimed it.
    if let Some(status) = journal.claimed(id)? {
        return Ok(status);
    }

    let state = journal.root_state(&plan.data)?;
    if state.pending.is_some() {
        return Err(DeployError::Busy);
    }
    if state.active.is_some() {
        return Err(DeployError::ActiveDeployment);
    }
    port.verify_root(&plan.installation, &plan.data)?;

    let _library = port.try_library_gate()?;
    if port.profile_revision(plan.profile)? != plan.revision {
        return Err(DeployError::StalePlan);
    }

    // Every destination and source is validated before a private snapshot or a
    // public mutation. A changed original refuses the whole plan.
    for intent in &plan.intents {
        let bound = port
            .resolve_target(&intent.target.target)
            .map_err(map_root_error)?;
        if bound != intent.target {
            return Err(DeployError::TargetUnavailable);
        }
        let observed = match port.observe(&bound) {
            Observation::Absent => PreState::Absent,
            Observation::Regular(fingerprint) => PreState::Regular(fingerprint),
            Observation::Unavailable(error) => return Err(map_root_error(error)),
        };
        if observed != intent.original {
            return Err(DeployError::RecoveryConflict);
        }
        let source = port.source(intent.source.installed, &intent.source.path)?;
        if (source.sha256, source.len) != (intent.source.sha256, intent.source.len) {
            return Err(DeployError::StalePlan);
        }
    }

    let op = journal.claim(Claim::Deploy(plan.clone()))?;
    let mut steps = Vec::with_capacity(plan.intents.len());
    for (ordinal, intent) in plan.intents.iter().enumerate() {
        let ordinal = u32::try_from(ordinal).map_err(|_| DeployError::Io)?;
        let spec = StepSpec {
            target: intent.target.clone(),
            original: PreImage {
                observation: intent.original.clone(),
                backup: None,
            },
            pre: intent.original.clone(),
            output: StepOutput::Source(intent.source.clone()),
        };
        match port.prepare(op, ordinal, StepDirection::Forward, &spec) {
            Ok(step) => steps.push(step),
            Err(error) => {
                let _ = journal.finish(op, Finalize::Cancelled);
                return Err(error);
            }
        }
    }

    if let Err(error) = journal.seal(op, steps.clone()) {
        let _ = journal.finish(op, Finalize::Cancelled);
        return Err(error);
    }

    // Cancellation is supported only before the public-write phase.
    if cancelled() {
        return finish(journal, op, Finalize::Cancelled);
    }

    validate_prepared(journal, port, op, &steps)?;
    journal.set_phase(op, Phase::Prepared, Phase::Applying)?;
    drop(_library);

    for step in &steps {
        journal.record_step(
            op,
            step.ordinal,
            StepPhase::Ready,
            StepPhase::Attempting,
            None,
        )?;
        inspect_effect_ready(journal, port, op, step)?;
        let outcome = execute(port, step)?;
        settle_forward(journal, port, op, step, outcome)?;
    }

    // Recheck every visible target, consumed stage and retained pre-image
    // without pretending later external edits are excluded.
    for step in &steps {
        let evidence = crate::recovery::inspect_checked(journal, port, step)?;
        if let Err(error) = crate::recovery::check_post(port, step, &evidence) {
            journal.block(op, Some(error))?;
            return Err(error);
        }
    }

    finish(journal, op, Finalize::DeployCommitted)
}

/// Restore the committed generation in reverse canonical order.
///
/// With no active generation this is a typed no-op and no operation is
/// allocated. A pending Restore reports its tracked status instead of starting
/// a second one.
pub fn restore<J: Journal, P: DeploymentInputs>(
    journal: &mut J,
    port: &mut P,
    installation: &RootIdentity,
    data: &RootIdentity,
) -> Result<OperationStatus, DeployError> {
    let _root = port.try_root_gate()?;
    port.verify_root(installation, data)?;

    let state = journal.root_state(data)?;
    if let Some(op) = state.pending {
        journal.verify_pending_pair(op, installation, data)?;
        let status = journal.status(op)?.ok_or(DeployError::UnknownOperation)?;
        if status.kind == OperationKind::Restore {
            return Ok(status);
        }
        return Err(DeployError::Busy);
    }
    let generation = state.active.ok_or(DeployError::NoActiveDeployment)?;
    let record = journal.generation(generation)?;

    let _library = port.try_library_gate()?;

    // Build every reverse step and validate originals and current managed
    // identities before claiming or touching a destination.
    let mut specs = Vec::with_capacity(record.steps.len());
    for row in record.steps.iter().rev() {
        let forward = &row.prepared;
        let managed = expected_post_state(forward);
        let PreState::Regular(managed_fingerprint) = &managed else {
            return Err(DeployError::RecoveryConflict);
        };
        let bound = port
            .resolve_target(&forward.spec.target.target)
            .map_err(map_root_error)?;
        if bound != forward.spec.target {
            return Err(DeployError::TargetUnavailable);
        }
        let observed = match port.observe(&bound) {
            Observation::Absent => PreState::Absent,
            Observation::Regular(fingerprint) => PreState::Regular(fingerprint),
            Observation::Unavailable(error) => return Err(map_root_error(error)),
        };
        if observed != managed {
            return Err(DeployError::RecoveryConflict);
        }

        let original = forward.spec.original.clone();
        let output = match &original.observation {
            PreState::Absent => StepOutput::Absent,
            PreState::Regular(fingerprint) => {
                let at = original.backup.ok_or(DeployError::RecoveryConflict)?;
                port.validate_backup(at, fingerprint)?;
                StepOutput::Original(at)
            }
        };
        specs.push(StepSpec {
            target: forward.spec.target.clone(),
            original,
            pre: PreState::Regular(managed_fingerprint.clone()),
            output,
        });
    }

    let op = journal.claim(Claim::Restore {
        installation: installation.clone(),
        data: data.clone(),
        generation,
    })?;

    let mut steps = Vec::with_capacity(specs.len());
    for (spec, row) in specs.into_iter().zip(record.steps.iter().rev()) {
        match port.prepare(op, row.prepared.ordinal, StepDirection::Forward, &spec) {
            Ok(step) => steps.push(step),
            Err(error) => {
                let _ = journal.finish(op, Finalize::Cancelled);
                return Err(error);
            }
        }
    }

    if let Err(error) = journal.seal(op, steps.clone()) {
        let _ = journal.finish(op, Finalize::Cancelled);
        return Err(error);
    }
    validate_prepared(journal, port, op, &steps)?;
    journal.set_phase(op, Phase::Prepared, Phase::Restoring)?;
    drop(_library);

    for step in &steps {
        journal.record_step(
            op,
            step.ordinal,
            StepPhase::Ready,
            StepPhase::Attempting,
            None,
        )?;
        inspect_effect_ready(journal, port, op, step)?;
        let outcome = execute(port, step)?;
        settle_forward(journal, port, op, step, outcome)?;
    }

    for step in &steps {
        let evidence = crate::recovery::inspect_checked(journal, port, step)?;
        if let Err(error) = crate::recovery::check_post(port, step, &evidence) {
            journal.block(op, Some(error))?;
            return Err(error);
        }
    }

    finish(journal, op, Finalize::RestoreCompleted)
}

/// Inspect a step at the per-step apply boundary. An unreadable target is a
/// typed blocker: the checked exchange cannot prove its precondition from a
/// failed read, so no effect may run and no displacement may be attributed.
///
/// The full precondition is repeated here, not only readability: a stage or
/// backup changed after the operation-wide preflight must block this effect
/// before any later step mutates the install.
fn inspect_effect_ready<J: Journal, P: CopyEffects>(
    journal: &mut J,
    port: &mut P,
    op: OperationId,
    step: &PreparedStep,
) -> Result<(), DeployError> {
    let evidence = crate::recovery::inspect_checked(journal, port, step)?;
    if let Some(error) = crate::recovery::target_error(&evidence) {
        journal.block(op, Some(error))?;
        return Err(error);
    }
    if let Err(error) = crate::recovery::check_pre(port, step, &evidence) {
        journal.block(op, Some(error))?;
        return Err(error);
    }
    Ok(())
}

fn validate_prepared<J: Journal, P: CopyEffects>(
    journal: &mut J,
    port: &mut P,
    op: OperationId,
    steps: &[PreparedStep],
) -> Result<(), DeployError> {
    for step in steps {
        let evidence = crate::recovery::inspect_checked(journal, port, step)?;
        if let Err(error) = crate::recovery::check_pre(port, step, &evidence) {
            journal.block(op, Some(error))?;
            return Err(error);
        }
    }
    Ok(())
}

pub fn operation_status<J: Journal>(
    journal: &mut J,
    id: OperationId,
) -> Result<OperationStatus, DeployError> {
    journal.status(id)?.ok_or(DeployError::UnknownOperation)
}

/// Execute one prepared forward (or restore) step through the checked port.
///
/// An invalid step shape is an internal invariant failure, not a native
/// capability result, so it fails with a stable `Io` code.
pub(crate) fn execute<P: CopyEffects>(
    port: &mut P,
    step: &PreparedStep,
) -> Result<EffectOutcome, DeployError> {
    let outcome = match (&step.spec.pre, &step.spec.output, step.staged.as_ref()) {
        (PreState::Absent, StepOutput::Source(_), Some((stage, _))) => {
            port.publish_new_absent(&step.spec.target, *stage)
        }
        (PreState::Regular(expected), StepOutput::Source(_), Some((stage, _))) => {
            port.replace_existing_retain_displaced(&step.spec.target, expected, *stage, step.retain)
        }
        (PreState::Regular(expected), StepOutput::Original(_), Some((stage, _))) => {
            port.replace_existing_retain_displaced(&step.spec.target, expected, *stage, step.retain)
        }
        (PreState::Regular(expected), StepOutput::Absent, _) => {
            port.detach_existing_retain(&step.spec.target, expected, step.retain)
        }
        _ => return Err(DeployError::Io),
    };
    Ok(outcome)
}

/// Execute the recorded undo step: restore the step pre-image or detach a
/// managed creation.
pub(crate) fn execute_undo<P: CopyEffects>(
    port: &mut P,
    undo: &PreparedStep,
) -> Result<EffectOutcome, DeployError> {
    let outcome = match (&undo.spec.pre, &undo.spec.output, undo.staged.as_ref()) {
        (PreState::Regular(expected), StepOutput::Original(_), Some((stage, _))) => {
            port.replace_existing_retain_displaced(&undo.spec.target, expected, *stage, undo.retain)
        }
        (PreState::Regular(expected), StepOutput::Absent, _) => {
            port.detach_existing_retain(&undo.spec.target, expected, undo.retain)
        }
        _ => return Err(DeployError::Io),
    };
    Ok(outcome)
}

/// The retained-artifact evidence promised by a settled step.
pub(crate) fn expected_retained(step: &PreparedStep) -> Artifact {
    match &step.spec.pre {
        PreState::Absent => Artifact::Absent,
        PreState::Regular(fingerprint) => Artifact::Regular(fingerprint.clone()),
    }
}

/// Fully settled evidence: expected target, consumed stage and the exact
/// retained pre-image (or explicit absence).
pub(crate) fn validate_settled(
    step: &PreparedStep,
    evidence: &Evidence,
) -> Result<(), DeployError> {
    if !crate::recovery::matches_post(step, evidence) {
        return Err(DeployError::RecoveryConflict);
    }
    Ok(())
}

/// The observation this prepared step promises after a successful effect.
pub(crate) fn expected_post(step: &PreparedStep) -> Observation {
    match &step.staged {
        Some((_, fingerprint)) => Observation::Regular(fingerprint.clone()),
        None => Observation::Absent,
    }
}

/// The pre/post state form of [`expected_post`], for comparisons with recorded
/// intents rather than live evidence.
pub(crate) fn expected_post_state(step: &PreparedStep) -> PreState {
    match &step.staged {
        Some((_, fingerprint)) => PreState::Regular(fingerprint.clone()),
        None => PreState::Absent,
    }
}

pub(crate) fn settle_forward<J: Journal, P: CopyEffects>(
    journal: &mut J,
    port: &mut P,
    op: OperationId,
    step: &PreparedStep,
    outcome: EffectOutcome,
) -> Result<(), DeployError> {
    let evidence = effect_evidence(journal, port, op, step, outcome)?;
    match port.durability_barrier(step, &evidence) {
        BarrierOutcome::Durable(evidence) => {
            crate::recovery::check_displacements(journal, port, step, &evidence)?;
            if let Err(error) = validate_settled(step, &evidence) {
                journal.block(op, Some(error))?;
                return Err(error);
            }
            journal.record_step(
                op,
                step.ordinal,
                StepPhase::Attempting,
                StepPhase::Applied,
                Some(evidence),
            )
        }
        BarrierOutcome::Unavailable(error) => {
            let error = map_root_error(error);
            journal.block(op, Some(error))?;
            Err(error)
        }
        BarrierOutcome::Ambiguous(evidence) => {
            crate::recovery::check_displacements(journal, port, step, &evidence)?;
            journal.block(op, Some(DeployError::Io))?;
            Err(DeployError::Io)
        }
    }
}

pub(crate) fn effect_evidence<J: Journal, P: CopyEffects>(
    journal: &mut J,
    port: &mut P,
    op: OperationId,
    step: &PreparedStep,
    outcome: EffectOutcome,
) -> Result<Evidence, DeployError> {
    // A success/ambiguity/conflict receipt is not an inventory of private
    // slots. Observe those independently, including the reserved alternate.
    let inspected = crate::recovery::inspect_checked(journal, port, step);
    if let EffectOutcome::Observed(evidence)
    | EffectOutcome::Conflict(evidence)
    | EffectOutcome::Ambiguous(evidence) = &outcome
    {
        if crate::recovery::preserve_canonical_displacement(journal, step, evidence)? {
            return Err(DeployError::RecoveryConflict);
        }
    }
    match outcome {
        EffectOutcome::Observed(evidence) => {
            inspected?;
            if let Err(error) = validate_settled(step, &evidence) {
                journal.block(op, Some(error))?;
                return Err(error);
            }
            Ok(evidence)
        }
        EffectOutcome::Displaced { evidence, at } => {
            if at != step.retain || validate_settled(step, &evidence).is_err() {
                journal.block_displacement(op, step, at, &evidence)?;
                inspected?;
                return Err(DeployError::RecoveryConflict);
            }
            inspected?;
            Ok(evidence)
        }
        EffectOutcome::Conflict(_) => {
            inspected?;
            journal.block(op, Some(DeployError::RecoveryConflict))?;
            Err(DeployError::RecoveryConflict)
        }
        EffectOutcome::Unavailable(error) => {
            inspected?;
            let error = map_root_error(error);
            journal.block(op, Some(error))?;
            Err(error)
        }
        EffectOutcome::Ambiguous(_) => {
            inspected?;
            journal.block(op, Some(DeployError::Io))?;
            Err(DeployError::Io)
        }
    }
}

/// One durable finalize, with ambiguous SQL acknowledgement reopened and
/// inspected rather than repeated.
pub(crate) fn finish<J: Journal>(
    journal: &mut J,
    op: OperationId,
    how: Finalize,
) -> Result<OperationStatus, DeployError> {
    match journal.finish(op, how) {
        Ok(status) => Ok(status),
        Err(DeployError::Io) => {
            journal.reopen()?;
            match journal.status(op)? {
                Some(status)
                    if match how {
                        Finalize::DeployCommitted => {
                            status.kind == OperationKind::Deploy && status.phase == Phase::Completed
                        }
                        Finalize::RestoreCompleted => {
                            status.kind == OperationKind::Restore
                                && status.phase == Phase::Completed
                        }
                        Finalize::RolledBack => status.phase == Phase::RolledBack,
                        Finalize::Cancelled => status.phase == Phase::Cancelled,
                    } =>
                {
                    Ok(status)
                }
                Some(status) if status.phase.is_terminal() => Err(DeployError::Io),
                Some(_) => {
                    journal.block(op, Some(DeployError::Io))?;
                    Err(DeployError::Io)
                }
                None => Err(DeployError::Io),
            }
        }
        Err(error) => Err(error),
    }
}
