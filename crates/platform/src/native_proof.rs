//! Hidden synthetic proof surface for the native effect port.
//!
//! This is not a production dispatcher or permission boundary. It exists so the
//! R03 witnesses can drive the same native candidates against an isolated
//! synthetic installation while keeping the live [`InstallationBinding`]
//! authority for every observation and effect.
//!
//! Diagnostic paths below are read-only proof witnesses. They are derived from
//! typed operation references, never from caller-supplied strings.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use marduk_core::deployment::{
    BoundTarget, DeployError, DeploymentInputs, OperationId, PreState, SourceRef,
};
use marduk_core::ids::{InstalledRevisionId, ProfileId, ProfileRevision};
use marduk_core::recovery::{
    Artifact, BackupRef, BackupSlot, BarrierOutcome, CopyEffects, EffectOutcome, Evidence,
    Observation, PreImage, PreparedStep, StageRef, StepDirection, StepOutput, StepSpec,
};
use marduk_core::root_identity::{
    Fingerprint, GameLayout, RootError, RootIdentity, TargetObservation, UnsupportedReason,
};
use marduk_core::targets::{RelativePath, TargetFile};

use crate::effects as fsops;
use crate::effects::fingerprint_path;
use crate::native;
use crate::{WriterGate, WriterGuard};

/// Deterministic native-boundary checkpoints. The proof harness installs a
/// child witness here; no checkpoint grants exclusion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Checkpoint {
    BeforeNativeEffect,
    NativeRename {
        from: RootIdentity,
        to: RootIdentity,
    },
    AfterNativeEffect {
        raw_error: Option<i32>,
    },
}

/// Injected ambiguity. `None` is the only normal behavior.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    None,
    LoseAcknowledgement,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct StepKey {
    operation: u64,
    ordinal: u32,
    undone: bool,
}

fn key(op: OperationId, ordinal: u32, direction: StepDirection) -> StepKey {
    StepKey {
        operation: op.get(),
        ordinal,
        undone: direction == StepDirection::Undo,
    }
}

struct StepRecord {
    staged: Option<Fingerprint>,
    /// A replace uses `RENAME_EXCHANGE`/`ReplaceFileW`, so its former stage
    /// slot is the canonical displaced-retain alias.
    exchange: bool,
    pre: PreState,
}

/// Held proof lease. It proves only that the synthetic session took its gate.
pub struct ProofGuard(#[allow(dead_code)] WriterGuard);

/// Synthetic, operation-owned proof session.
pub struct ProofSession {
    location: PathBuf,
    layout: GameLayout,
    binding: native::Binding,
    private: PathBuf,
    library: PathBuf,
    checkpoint: Box<dyn FnMut(Checkpoint) -> Fault>,
    records: BTreeMap<StepKey, StepRecord>,
    root_gate: WriterGate,
    library_gate: WriterGate,
}

impl std::fmt::Debug for ProofSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ProofSession(..)")
    }
}

impl ProofSession {
    /// Reopen the exact recorded installation/Data pair. A substituted or
    /// moved root is refused before any proof state is created.
    pub fn open(
        location: &Path,
        layout: &GameLayout,
        installation: &RootIdentity,
        data: &RootIdentity,
        private: &Path,
        library: &Path,
        checkpoint: Box<dyn FnMut(Checkpoint) -> Fault>,
    ) -> Result<Self, RootError> {
        let binding = native::reopen_installation(location, layout, installation, data)?;
        fs::create_dir_all(private).map_err(|_| RootError::Io)?;
        let root_gate = WriterGate::open(private.join("root-gate")).map_err(|_| RootError::Io)?;
        let library_gate =
            WriterGate::open(private.join("library-gate")).map_err(|_| RootError::Io)?;
        Ok(Self {
            location: location.to_path_buf(),
            layout: *layout,
            binding,
            private: private.to_path_buf(),
            library: library.to_path_buf(),
            checkpoint,
            records: BTreeMap::new(),
            root_gate,
            library_gate,
        })
    }

    pub fn stage_path(&self, at: StageRef) -> PathBuf {
        self.op_dir(at.operation().get(), at.ordinal(), at.direction())
            .join("stage")
    }

    pub fn backup_path(&self, at: BackupRef) -> PathBuf {
        let directory = self.op_dir(at.operation().get(), at.ordinal(), at.direction());
        match at.slot() {
            BackupSlot::Original => directory.join("original"),
            BackupSlot::Step => directory.join("step"),
            BackupSlot::AlternateDisplaced => directory.join("alternate"),
            BackupSlot::Displaced => {
                let exchange = self
                    .records
                    .get(&key(at.operation(), at.ordinal(), at.direction()))
                    .is_some_and(|record| record.exchange);
                if exchange {
                    directory.join("stage")
                } else {
                    directory.join("displaced")
                }
            }
        }
    }

    fn op_dir(&self, operation: u64, ordinal: u32, direction: StepDirection) -> PathBuf {
        self.private
            .join("ops")
            .join(operation.to_string())
            .join(ordinal.to_string())
            .join(match direction {
                StepDirection::Forward => "forward",
                StepDirection::Undo => "undo",
            })
    }

    fn target_path(&self, target: &BoundTarget) -> PathBuf {
        let mut components = target
            .target
            .relative
            .as_str()
            .split('/')
            .collect::<Vec<_>>();
        components.pop();
        let mut path = self.location.join(self.layout.data);
        for component in components {
            path.push(component);
        }
        path.push(&target.leaf);
        path
    }

    fn source_path(&self, installed: InstalledRevisionId, relative: &RelativePath) -> PathBuf {
        let mut path = self
            .library
            .join("revisions")
            .join(installed.get().to_string())
            .join("payload");
        for component in relative.as_str().split('/') {
            path.push(component);
        }
        path
    }

    fn fire(&mut self, checkpoint: Checkpoint) -> Fault {
        (self.checkpoint)(checkpoint)
    }

    fn resolved(&self, target: &BoundTarget) -> Result<native::ResolvedTarget<'_>, RootError> {
        self.binding.check_roots()?;
        let resolved = native::resolve_existing_parent(&self.binding, &target.target.relative)?;
        if resolved.parent_identity() != &target.parent || resolved.leaf() != target.leaf {
            return Err(RootError::OutsideBoundary);
        }
        Ok(resolved)
    }

    fn observe_bound(&self, target: &BoundTarget) -> Observation {
        match self.resolved(target) {
            Ok(resolved) => match native::observe_target(&resolved) {
                Ok(TargetObservation::Absent) => Observation::Absent,
                Ok(TargetObservation::Regular { fingerprint }) => Observation::Regular(fingerprint),
                Ok(TargetObservation::Unsupported { reason }) => {
                    Observation::Unavailable(RootError::UnsupportedTarget(reason))
                }
                Err(error) => Observation::Unavailable(error),
            },
            Err(error) => Observation::Unavailable(error),
        }
    }

    /// Observe the public target and both operation-owned slots. The logical
    /// stage is absent once an exchange moved its object to the target.
    fn evidence(&self, target: &BoundTarget, key: StepKey) -> Evidence {
        let Some(record) = self.records.get(&key) else {
            return Evidence {
                target: self.observe_bound(target),
                stage: Artifact::Unavailable,
                retained: Artifact::Unavailable,
            };
        };
        let directory = self.op_dir(key.operation, key.ordinal, direction(key));
        let stage_observation = artifact(&directory.join("stage"));
        let (stage, retained) = if record.exchange {
            // The exchange renames the staged object onto the public target, so
            // the former stage slot is now the displaced artifact. The logical
            // stage survives only while the slot still holds the staged bytes;
            // otherwise those bytes are the retention evidence.
            match &stage_observation {
                Artifact::Regular(actual) if record.staged.as_ref() == Some(actual) => {
                    (Artifact::Regular(actual.clone()), Artifact::Absent)
                }
                other => (Artifact::Absent, other.clone()),
            }
        } else {
            (stage_observation, artifact(&directory.join("displaced")))
        };
        Evidence {
            target: self.observe_bound(target),
            stage,
            retained,
        }
    }

    fn validate_backup_inner(
        &self,
        at: BackupRef,
        expected: &Fingerprint,
    ) -> Result<(), DeployError> {
        let path = self.backup_path(at);
        match fingerprint_path(&path) {
            Ok(actual)
                if actual.len == expected.len
                    && actual.sha256 == expected.sha256
                    && actual.metadata == expected.metadata =>
            {
                Ok(())
            }
            Ok(_) => Err(DeployError::RecoveryConflict),
            Err(RootError::TargetUnavailable) => Err(DeployError::TargetUnavailable),
            Err(RootError::UnsupportedTarget(_)) => Err(DeployError::UnsupportedTarget),
            Err(_) => Err(DeployError::Io),
        }
    }

    fn exchange_target(
        &mut self,
        target: &BoundTarget,
        expected: &Fingerprint,
        staged: StageRef,
        retain: BackupRef,
    ) -> EffectOutcome {
        let key = key(staged.operation(), staged.ordinal(), staged.direction());
        let Some(record) = self.records.get(&key) else {
            return EffectOutcome::Unavailable(RootError::TargetUnavailable);
        };
        let Some(staged_fingerprint) = record.staged.clone() else {
            return EffectOutcome::Unavailable(RootError::TargetUnavailable);
        };
        if record.pre != PreState::Regular(expected.clone())
            || retain
                != BackupRef::from_record(
                    staged.operation(),
                    staged.ordinal(),
                    staged.direction(),
                    BackupSlot::Displaced,
                )
        {
            return EffectOutcome::Conflict(self.evidence(target, key));
        }
        let stage_path = self.stage_path(staged);
        let stage_directory = stage_path
            .parent()
            .expect("operation-owned stage has a parent")
            .to_path_buf();

        // Preflight: the target must be readable and the staged output intact.
        // The bound pre-image is checked only after the race checkpoint.
        match self.observe_bound(target) {
            Observation::Unavailable(error) => return EffectOutcome::Unavailable(error),
            _ => {}
        }
        if artifact(&stage_path) != Artifact::Regular(staged_fingerprint.clone()) {
            return EffectOutcome::Conflict(self.evidence(target, key));
        }

        self.fire(Checkpoint::BeforeNativeEffect);

        let (target_directory, leaf, target_parent_identity, target_path) =
            match self.resolved(target) {
                Ok(resolved) => (
                    match resolved.parent_dir() {
                        Ok(directory) => directory,
                        Err(error) => return EffectOutcome::Unavailable(error),
                    },
                    resolved.leaf().to_owned(),
                    resolved.parent_identity().clone(),
                    self.target_path(target),
                ),
                Err(error) => return EffectOutcome::Unavailable(error),
            };
        if self.observe_bound(target) != Observation::Regular(expected.clone())
            || artifact(&stage_path) != Artifact::Regular(staged_fingerprint.clone())
        {
            return EffectOutcome::Conflict(self.evidence(target, key));
        }
        let Ok(stage_parent) = fsops::directory_identity(&stage_directory) else {
            return EffectOutcome::Unavailable(RootError::TargetUnavailable);
        };
        if !same_rename_domain(&stage_parent, &target_parent_identity) {
            return EffectOutcome::Unavailable(RootError::OutsideBoundary);
        }
        let stage_dir = match fsops::open_directory(&stage_directory) {
            Ok(directory) => directory,
            Err(error) => return EffectOutcome::Unavailable(error),
        };
        let retain_path = self.backup_path(retain);

        self.fire(Checkpoint::NativeRename {
            from: stage_parent,
            to: target_parent_identity,
        });
        let result = fsops::replace(
            &stage_dir,
            "stage",
            &target_directory,
            &leaf,
            &stage_path,
            &target_path,
            &retain_path,
        );
        let raw_error = result.as_ref().err().and_then(io::Error::raw_os_error);
        let fault = self.fire(Checkpoint::AfterNativeEffect { raw_error });
        let evidence = self.evidence(target, key);

        if let Err(_error) = result {
            return match evidence.target {
                Observation::Unavailable(error) => EffectOutcome::Unavailable(error),
                _ if evidence.target == Observation::Regular(expected.clone())
                    && evidence.stage == Artifact::Regular(staged_fingerprint.clone())
                    && evidence.retained == Artifact::Absent =>
                {
                    EffectOutcome::Conflict(evidence)
                }
                _ => EffectOutcome::Ambiguous(evidence),
            };
        }

        if fault == Fault::LoseAcknowledgement {
            return EffectOutcome::Ambiguous(evidence);
        }
        if evidence.target == Observation::Regular(staged_fingerprint)
            && evidence.stage == Artifact::Absent
        {
            match &evidence.retained {
                Artifact::Regular(actual) if actual == expected => {
                    return EffectOutcome::Observed(evidence);
                }
                // A report may name only bytes that are actually retained;
                // absent bytes are an unproven outcome, not a displacement.
                Artifact::Regular(_) => {
                    return EffectOutcome::Displaced {
                        evidence,
                        at: retain,
                    };
                }
                _ => return EffectOutcome::Ambiguous(evidence),
            }
        }
        EffectOutcome::Ambiguous(evidence)
    }

    fn publish_absent(&mut self, target: &BoundTarget, staged: StageRef) -> EffectOutcome {
        let key = key(staged.operation(), staged.ordinal(), staged.direction());
        let Some(record) = self.records.get(&key) else {
            return EffectOutcome::Unavailable(RootError::TargetUnavailable);
        };
        let Some(staged_fingerprint) = record.staged.clone() else {
            return EffectOutcome::Unavailable(RootError::TargetUnavailable);
        };
        let stage_path = self.stage_path(staged);
        let stage_directory = stage_path
            .parent()
            .expect("operation-owned stage has a parent")
            .to_path_buf();

        match self.observe_bound(target) {
            Observation::Unavailable(error) => return EffectOutcome::Unavailable(error),
            _ => {}
        }
        if artifact(&stage_path) != Artifact::Regular(staged_fingerprint.clone()) {
            return EffectOutcome::Conflict(self.evidence(target, key));
        }

        self.fire(Checkpoint::BeforeNativeEffect);

        let (target_directory, leaf, target_parent_identity, target_path) =
            match self.resolved(target) {
                Ok(resolved) => (
                    match resolved.parent_dir() {
                        Ok(directory) => directory,
                        Err(error) => return EffectOutcome::Unavailable(error),
                    },
                    resolved.leaf().to_owned(),
                    resolved.parent_identity().clone(),
                    self.target_path(target),
                ),
                Err(error) => return EffectOutcome::Unavailable(error),
            };
        let observed_after = self.observe_bound(target);
        if observed_after != Observation::Absent
            || artifact(&stage_path) != Artifact::Regular(staged_fingerprint.clone())
        {
            let evidence = self.evidence(target, key);
            return match observed_after {
                Observation::Unavailable(error) => EffectOutcome::Unavailable(error),
                Observation::Absent => EffectOutcome::Unavailable(RootError::Io),
                _ => EffectOutcome::Conflict(evidence),
            };
        }
        let Ok(stage_parent) = fsops::directory_identity(&stage_directory) else {
            return EffectOutcome::Unavailable(RootError::TargetUnavailable);
        };
        if !same_rename_domain(&stage_parent, &target_parent_identity) {
            return EffectOutcome::Unavailable(RootError::OutsideBoundary);
        }
        let stage_dir = match fsops::open_directory(&stage_directory) {
            Ok(directory) => directory,
            Err(error) => return EffectOutcome::Unavailable(error),
        };

        self.fire(Checkpoint::NativeRename {
            from: stage_parent,
            to: target_parent_identity,
        });
        let result = fsops::publish(
            &stage_dir,
            "stage",
            &target_directory,
            &leaf,
            &stage_path,
            &target_path,
        );
        let raw_error = result.as_ref().err().and_then(io::Error::raw_os_error);
        let fault = self.fire(Checkpoint::AfterNativeEffect { raw_error });
        let evidence = self.evidence(target, key);

        if let Err(_error) = result {
            return match &evidence.target {
                Observation::Unavailable(error) => EffectOutcome::Unavailable(*error),
                Observation::Absent
                    if evidence.stage == Artifact::Regular(staged_fingerprint.clone())
                        && evidence.retained == Artifact::Absent =>
                {
                    EffectOutcome::Conflict(evidence)
                }
                Observation::Absent => EffectOutcome::Ambiguous(evidence),
                // The staged bytes at the public name mean the move applied
                // despite the reported error; that is not a conflict.
                Observation::Regular(actual) if *actual == staged_fingerprint => {
                    EffectOutcome::Ambiguous(evidence)
                }
                Observation::Regular(_) => EffectOutcome::Conflict(evidence),
            };
        }
        if fault == Fault::LoseAcknowledgement {
            return EffectOutcome::Ambiguous(evidence);
        }
        if evidence.target == Observation::Regular(staged_fingerprint)
            && evidence.stage == Artifact::Absent
            && evidence.retained == Artifact::Absent
        {
            EffectOutcome::Observed(evidence)
        } else {
            EffectOutcome::Ambiguous(evidence)
        }
    }

    fn detach(
        &mut self,
        target: &BoundTarget,
        expected: &Fingerprint,
        retain: BackupRef,
    ) -> EffectOutcome {
        let key = key(retain.operation(), retain.ordinal(), retain.direction());
        let Some(record) = self.records.get(&key) else {
            return EffectOutcome::Unavailable(RootError::TargetUnavailable);
        };
        if record.pre != PreState::Regular(expected.clone())
            || retain
                != BackupRef::from_record(
                    retain.operation(),
                    retain.ordinal(),
                    retain.direction(),
                    BackupSlot::Displaced,
                )
        {
            return EffectOutcome::Conflict(self.evidence(target, key));
        }
        let displaced_path = self.backup_path(retain);
        let displaced_directory = displaced_path
            .parent()
            .expect("operation-owned retain has a parent")
            .to_path_buf();

        match self.observe_bound(target) {
            Observation::Unavailable(error) => return EffectOutcome::Unavailable(error),
            _ => {}
        }
        if artifact(&displaced_path) != Artifact::Absent {
            return EffectOutcome::Conflict(self.evidence(target, key));
        }

        self.fire(Checkpoint::BeforeNativeEffect);

        let (target_directory, leaf, target_parent_identity, target_path) =
            match self.resolved(target) {
                Ok(resolved) => (
                    match resolved.parent_dir() {
                        Ok(directory) => directory,
                        Err(error) => return EffectOutcome::Unavailable(error),
                    },
                    resolved.leaf().to_owned(),
                    resolved.parent_identity().clone(),
                    self.target_path(target),
                ),
                Err(error) => return EffectOutcome::Unavailable(error),
            };
        if self.observe_bound(target) != Observation::Regular(expected.clone())
            || artifact(&displaced_path) != Artifact::Absent
        {
            return EffectOutcome::Conflict(self.evidence(target, key));
        }
        let Ok(displaced_parent) = fsops::directory_identity(&displaced_directory) else {
            return EffectOutcome::Unavailable(RootError::TargetUnavailable);
        };
        if !same_rename_domain(&target_parent_identity, &displaced_parent) {
            return EffectOutcome::Unavailable(RootError::OutsideBoundary);
        }
        let displaced_dir = match fsops::open_directory(&displaced_directory) {
            Ok(directory) => directory,
            Err(error) => return EffectOutcome::Unavailable(error),
        };

        self.fire(Checkpoint::NativeRename {
            from: target_parent_identity,
            to: displaced_parent,
        });
        let result = fsops::detach(
            &target_directory,
            &leaf,
            &displaced_dir,
            "displaced",
            &target_path,
            &displaced_path,
        );
        let raw_error = result.as_ref().err().and_then(io::Error::raw_os_error);
        let fault = self.fire(Checkpoint::AfterNativeEffect { raw_error });
        let evidence = self.evidence(target, key);

        if let Err(_error) = result {
            return match evidence.target {
                Observation::Unavailable(error) => EffectOutcome::Unavailable(error),
                _ if evidence.target == Observation::Regular(expected.clone())
                    && evidence.stage == Artifact::Absent
                    && evidence.retained == Artifact::Absent =>
                {
                    EffectOutcome::Conflict(evidence)
                }
                _ => EffectOutcome::Ambiguous(evidence),
            };
        }
        if fault == Fault::LoseAcknowledgement {
            return EffectOutcome::Ambiguous(evidence);
        }
        if evidence.target == Observation::Absent
            && evidence.stage == Artifact::Absent
            && evidence.retained == Artifact::Regular(expected.clone())
        {
            EffectOutcome::Observed(evidence)
        } else {
            EffectOutcome::Ambiguous(evidence)
        }
    }
}

fn direction(key: StepKey) -> StepDirection {
    if key.undone {
        StepDirection::Undo
    } else {
        StepDirection::Forward
    }
}

fn artifact(path: &Path) -> Artifact {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Artifact::Unsupported(UnsupportedReason::ObjectKind)
        }
        Ok(_) => match fingerprint_path(path) {
            Ok(fingerprint) => Artifact::Regular(fingerprint),
            Err(RootError::TargetUnavailable) => Artifact::Absent,
            Err(RootError::UnsupportedTarget(reason)) => Artifact::Unsupported(reason),
            Err(_) => Artifact::Unavailable,
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => Artifact::Absent,
        Err(_) => Artifact::Unavailable,
    }
}

fn same_rename_domain(left: &RootIdentity, right: &RootIdentity) -> bool {
    use marduk_core::root_identity::NativeIdentity;
    match (left.evidence(), right.evidence()) {
        (
            NativeIdentity::Linux {
                fs_magic: left_magic,
                fs_id: left_id,
                view_id: left_view,
                tree_id: left_tree,
                ..
            },
            NativeIdentity::Linux {
                fs_magic: right_magic,
                fs_id: right_id,
                view_id: right_view,
                tree_id: right_tree,
                ..
            },
        ) => {
            left_magic == right_magic
                && left_id == right_id
                && left_view == right_view
                && left_tree == right_tree
        }
        (
            NativeIdentity::Windows {
                volume_serial: left_volume,
                ..
            },
            NativeIdentity::Windows {
                volume_serial: right_volume,
                ..
            },
        ) => left_volume == right_volume,
        _ => false,
    }
}

fn map_root(error: RootError) -> DeployError {
    match error {
        RootError::TargetUnavailable => DeployError::TargetUnavailable,
        RootError::MissingParent | RootError::AmbiguousName | RootError::OutsideBoundary => {
            DeployError::InvalidPath
        }
        RootError::UnsupportedTarget(_) => DeployError::UnsupportedTarget,
        RootError::Io => DeployError::Io,
    }
}

impl CopyEffects for ProofSession {
    fn observe(&mut self, target: &BoundTarget) -> Observation {
        self.observe_bound(target)
    }

    fn prepare(
        &mut self,
        op: OperationId,
        ordinal: u32,
        direction: StepDirection,
        intent: &StepSpec,
    ) -> Result<PreparedStep, DeployError> {
        let key = key(op, ordinal, direction);
        if self.records.contains_key(&key) {
            return Err(DeployError::RecoveryConflict);
        }
        let directory = self.op_dir(op.get(), ordinal, direction);
        fs::create_dir_all(&directory).map_err(|_| DeployError::Io)?;

        let resolved = self.resolved(&intent.target).map_err(map_root)?;
        let observed = native::observe_target(&resolved).map_err(map_root)?;
        let pre = match observed {
            TargetObservation::Absent => PreState::Absent,
            TargetObservation::Regular { fingerprint } => PreState::Regular(fingerprint),
            TargetObservation::Unsupported { .. } => {
                return Err(DeployError::UnsupportedTarget);
            }
        };
        if pre != intent.pre {
            return Err(DeployError::RecoveryConflict);
        }
        let target_path = self.target_path(&intent.target);

        let mut spec = intent.clone();
        match (&intent.original.observation, intent.original.backup) {
            (PreState::Absent, None) => {}
            (PreState::Regular(expected), None) => {
                let at = BackupRef::from_record(op, ordinal, direction, BackupSlot::Original);
                copy_file(&target_path, &self.backup_path(at))?;
                self.validate_backup_inner(at, expected)?;
                spec.original.backup = Some(at);
            }
            (PreState::Regular(expected), Some(at)) => {
                self.validate_backup_inner(at, expected)?;
            }
            _ => return Err(DeployError::RecoveryConflict),
        }

        let step_pre = match &pre {
            PreState::Absent => PreImage {
                observation: PreState::Absent,
                backup: None,
            },
            PreState::Regular(fingerprint) => {
                let at = BackupRef::from_record(op, ordinal, direction, BackupSlot::Step);
                copy_file(&target_path, &self.backup_path(at))?;
                self.validate_backup_inner(at, fingerprint)?;
                PreImage {
                    observation: PreState::Regular(fingerprint.clone()),
                    backup: Some(at),
                }
            }
        };

        let staged = match &intent.output {
            StepOutput::Absent => None,
            StepOutput::Source(source) => {
                let source_path = self.source_path(source.installed, &source.path);
                let source_observed =
                    fingerprint_path(&source_path).map_err(|_| DeployError::StalePlan)?;
                if source_observed.len != source.len || source_observed.sha256 != source.sha256 {
                    return Err(DeployError::StalePlan);
                }
                let stage_path = self.stage_path(StageRef::from_record(op, ordinal, direction));
                copy_file(&source_path, &stage_path)?;
                let fingerprint = fingerprint_path(&stage_path).map_err(|_| DeployError::Io)?;
                if fingerprint.len != source.len || fingerprint.sha256 != source.sha256 {
                    return Err(DeployError::Io);
                }
                Some(fingerprint)
            }
            StepOutput::Original(at) => {
                let PreState::Regular(expected) = &intent.original.observation else {
                    return Err(DeployError::RecoveryConflict);
                };
                self.validate_backup_inner(*at, expected)?;
                let stage_path = self.stage_path(StageRef::from_record(op, ordinal, direction));
                copy_file(&self.backup_path(*at), &stage_path)?;
                let fingerprint = fingerprint_path(&stage_path).map_err(|_| DeployError::Io)?;
                if fingerprint.len != expected.len
                    || fingerprint.sha256 != expected.sha256
                    || fingerprint.metadata != expected.metadata
                {
                    return Err(DeployError::RecoveryConflict);
                }
                Some(fingerprint)
            }
        };

        let exchange = matches!(pre, PreState::Regular(_)) && staged.is_some();
        if !exchange && artifact(&directory.join("displaced")) != Artifact::Absent {
            return Err(DeployError::RecoveryConflict);
        }
        if let Some(_fingerprint) = &staged {
            let stage_directory = directory.to_path_buf();
            let target_parent = self
                .resolved(&intent.target)
                .map_err(map_root)?
                .parent_identity()
                .clone();
            let stage_parent = fsops::directory_identity(&stage_directory).map_err(map_root)?;
            if !same_rename_domain(&stage_parent, &target_parent) {
                return Err(DeployError::UnsupportedTarget);
            }
        }

        let retain = BackupRef::from_record(op, ordinal, direction, BackupSlot::Displaced);
        let prepared = PreparedStep {
            op,
            ordinal,
            direction,
            spec,
            step_pre,
            staged: staged
                .map(|fingerprint| (StageRef::from_record(op, ordinal, direction), fingerprint)),
            retain,
        };
        self.records.insert(
            key,
            StepRecord {
                staged: prepared
                    .staged
                    .as_ref()
                    .map(|(_, fingerprint)| fingerprint.clone()),
                exchange,
                pre: intent.pre.clone(),
            },
        );
        Ok(prepared)
    }

    fn publish_new_absent(&mut self, target: &BoundTarget, staged: StageRef) -> EffectOutcome {
        self.publish_absent(target, staged)
    }

    fn replace_existing_retain_displaced(
        &mut self,
        target: &BoundTarget,
        expected: &Fingerprint,
        staged: StageRef,
        retain: BackupRef,
    ) -> EffectOutcome {
        self.exchange_target(target, expected, staged, retain)
    }

    fn detach_existing_retain(
        &mut self,
        target: &BoundTarget,
        expected: &Fingerprint,
        retain: BackupRef,
    ) -> EffectOutcome {
        self.detach(target, expected, retain)
    }

    fn inspect_step(&mut self, step: &PreparedStep) -> Evidence {
        self.evidence(
            &step.spec.target,
            key(step.op, step.ordinal, step.direction),
        )
    }

    fn inspect_alternate_displacement(&mut self, step: &PreparedStep) -> Artifact {
        let key = key(step.op, step.ordinal, step.direction);
        if !self.records.contains_key(&key) {
            // Same missing-record fallback as `inspect_step`: no record means
            // no operation-owned slot to attribute.
            return Artifact::Unavailable;
        }
        artifact(&self.backup_path(step.alternate_displacement()))
    }

    fn durability_barrier(&mut self, step: &PreparedStep, observed: &Evidence) -> BarrierOutcome {
        let key = key(step.op, step.ordinal, step.direction);
        let current = self.evidence(&step.spec.target, key);
        if current != *observed {
            return BarrierOutcome::Ambiguous(current);
        }
        if cfg!(windows) {
            // No documented non-admin Windows namespace barrier is qualified.
            return BarrierOutcome::Unavailable(RootError::UnsupportedTarget(
                UnsupportedReason::Configuration,
            ));
        }
        if let Err(error) = self.sync_effect(step, key) {
            return BarrierOutcome::Unavailable(error);
        }
        let after = self.evidence(&step.spec.target, key);
        if after == current {
            BarrierOutcome::Durable(after)
        } else {
            BarrierOutcome::Ambiguous(after)
        }
    }

    fn validate_backup(
        &mut self,
        at: BackupRef,
        expected: &Fingerprint,
    ) -> Result<(), DeployError> {
        self.validate_backup_inner(at, expected)
    }
}

impl ProofSession {
    fn sync_effect(&self, step: &PreparedStep, key: StepKey) -> Result<(), RootError> {
        let directory = self.op_dir(key.operation, key.ordinal, direction(key));
        for name in ["stage", "displaced", "original", "step"] {
            let path = directory.join(name);
            if fs::symlink_metadata(&path).is_ok() {
                let file = fsops::open_file(&path)?;
                file.sync_all().map_err(|_| RootError::Io)?;
            }
        }
        if let Observation::Regular(_) = self.observe_bound(&step.spec.target) {
            let file = fsops::open_file(&self.target_path(&step.spec.target))?;
            file.sync_all().map_err(|_| RootError::Io)?;
        }
        fsops::sync_directory(&directory)?;
        let mut current = directory.parent();
        while let Some(path) = current {
            fsops::sync_directory(path)?;
            if path == self.private {
                break;
            }
            current = path.parent();
        }
        // The changed target parent is part of the barrier. A failed
        // re-resolution or open must not leave the effect reported durable.
        let resolved = self.resolved(&step.spec.target)?;
        let target_directory = resolved.parent_dir()?;
        target_directory.sync_all().map_err(|_| RootError::Io)?;
        Ok(())
    }
}

impl DeploymentInputs for ProofSession {
    type RootGuard = ProofGuard;
    type LibraryGuard = ProofGuard;

    fn try_root_gate(&mut self) -> Result<Self::RootGuard, DeployError> {
        self.root_gate
            .try_lock()
            .map(ProofGuard)
            .map_err(|error| match error {
                crate::GateError::Busy => DeployError::Busy,
                _ => DeployError::TargetUnavailable,
            })
    }

    fn try_library_gate(&mut self) -> Result<Self::LibraryGuard, DeployError> {
        self.library_gate
            .try_lock()
            .map(ProofGuard)
            .map_err(|error| match error {
                crate::GateError::Busy => DeployError::Busy,
                _ => DeployError::TargetUnavailable,
            })
    }

    fn verify_root(
        &mut self,
        installation: &RootIdentity,
        data: &RootIdentity,
    ) -> Result<(), DeployError> {
        self.binding.check_roots().map_err(map_root)?;
        let (actual_installation, actual_data) = self.binding.identities();
        if actual_installation == installation && actual_data == data {
            Ok(())
        } else {
            Err(DeployError::TargetUnavailable)
        }
    }

    fn resolve_target(&mut self, target: &TargetFile) -> Result<BoundTarget, RootError> {
        self.binding.check_roots()?;
        let resolved = native::resolve_existing_parent(&self.binding, &target.relative)?;
        Ok(BoundTarget {
            target: target.clone(),
            parent: resolved.parent_identity().clone(),
            leaf: resolved.leaf().to_owned(),
        })
    }

    fn profile_revision(&mut self, _profile: ProfileId) -> Result<ProfileRevision, DeployError> {
        Ok(ProfileRevision::new(1))
    }

    fn source(
        &mut self,
        installed: InstalledRevisionId,
        path: &RelativePath,
    ) -> Result<SourceRef, DeployError> {
        let source_path = self.source_path(installed, path);
        let fingerprint = fingerprint_path(&source_path).map_err(|error| match error {
            RootError::TargetUnavailable => DeployError::StalePlan,
            RootError::UnsupportedTarget(_) => DeployError::UnsupportedTarget,
            RootError::Io => DeployError::Io,
            _ => DeployError::InvalidPath,
        })?;
        Ok(SourceRef {
            installed,
            path: path.clone(),
            sha256: fingerprint.sha256,
            len: fingerprint.len,
        })
    }
}

fn copy_file(source: &Path, destination: &Path) -> Result<(), DeployError> {
    fsops::copy_file(source, destination)
        .map(|_| ())
        .map_err(|error| match error {
            RootError::TargetUnavailable => DeployError::TargetUnavailable,
            RootError::UnsupportedTarget(_) => DeployError::UnsupportedTarget,
            _ => DeployError::Io,
        })
}
