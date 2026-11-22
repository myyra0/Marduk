//! Native checked copy adapter for qualified volumes.
//!
//! The journal owns the durable operation; this adapter owns the live
//! installation binding, the operation gates and the native effect session. It
//! is composed by the workspace only when both the installation and library
//! roots report runtime evidence through [`marduk_platform::effects::volume_qualified`].
//! An unqualified root is a typed refusal before any private or public write.
//!
//! Every public effect re-proves the bound root pair, re-observes the target and
//! the operation-owned slots, and classifies an error by fresh evidence rather
//! than treating it as "no change". A recorded installation locator mismatch is
//! refused; the adapter never silently re-binds another installation.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use marduk_core::deployment::{
    BoundTarget, DeployError, DeploymentInputs, OperationId, PreState, SourceRef,
};
use marduk_core::ids::{InstalledRevisionId, ProfileId, ProfileRevision};
use marduk_core::recovery::{
    Artifact, BackupRef, BackupSlot, BarrierOutcome, CopyEffects, EffectOutcome, Evidence,
    Observation, PreImage, PreparedStep, StageRef, StepDirection, StepOutput, StepSpec,
};
use marduk_core::root_identity::{
    FileMetadata, Fingerprint, GameLayout, NativeIdentity, RootError, RootIdentity,
    TargetObservation, UnsupportedReason,
};
use marduk_core::targets::{RelativePath, TargetFile};
use marduk_platform::effects;
use marduk_platform::{GateError, InstallationBinding, WriterGate, WriterGuard};
use rusqlite::{Connection, OpenFlags};

/// Live native effect session for one deployment or recovery call.
pub struct NativeEffects {
    root_handle: File,
    locator: PathBuf,
    layout: GameLayout,
    binding: InstallationBinding,
    private: PathBuf,
    library: PathBuf,
    root_gate: WriterGate,
    library_gate: WriterGate,
    /// The staged fingerprint for each inspected step. Effects need it to
    /// distinguish "stage untouched" from "stage displaced the target". The
    /// value is copied from the sealed step whenever recovery or apply inspects
    /// it, so a restart does not depend on the map that `prepare` built.
    staged: BTreeMap<(u64, u32, StepDirection), Fingerprint>,
}

impl std::fmt::Debug for NativeEffects {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("NativeEffects(..)")
    }
}

impl NativeEffects {
    /// Bind the selected installation and prepare the app-owned effect area.
    ///
    /// Both roots must carry recorded native effect evidence. A refused volume
    /// returns `UnsupportedTarget` before the binding is opened or any
    /// directory is created.
    pub fn open(app_data: &Path, locator: &Path, layout: GameLayout) -> Result<Self, DeployError> {
        if !effects::volume_qualified(app_data) || !effects::volume_qualified(locator) {
            return Err(DeployError::UnsupportedTarget);
        }
        let binding = marduk_platform::bind_installation(locator, &layout).map_err(map_root)?;
        let root_handle =
            marduk_platform::open_private_directory(app_data).map_err(|_| DeployError::Io)?;
        drop(
            marduk_platform::open_private_child_directory(&root_handle, "effects")
                .map_err(|_| DeployError::Io)?,
        );
        let private = marduk_platform::private_child_path(&root_handle, "effects")
            .map_err(|_| DeployError::Io)?;
        let root_gate =
            WriterGate::open_at(&root_handle, crate::schema::ROOT_LOCK_FILE).map_err(gate_error)?;
        let library_gate =
            WriterGate::open_at(&root_handle, crate::schema::LOCK_FILE).map_err(gate_error)?;
        Ok(Self {
            root_handle,
            locator: locator.to_path_buf(),
            layout,
            binding,
            private,
            library: app_data.to_path_buf(),
            root_gate,
            library_gate,
            staged: BTreeMap::new(),
        })
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

    fn stage_path(&self, at: StageRef) -> PathBuf {
        self.op_dir(at.operation().get(), at.ordinal(), at.direction())
            .join("stage")
    }

    fn backup_path(&self, at: BackupRef) -> PathBuf {
        let directory = self.op_dir(at.operation().get(), at.ordinal(), at.direction());
        match at.slot() {
            BackupSlot::Original => directory.join("original"),
            BackupSlot::Step => directory.join("step"),
            BackupSlot::Displaced => directory.join("displaced"),
            BackupSlot::AlternateDisplaced => directory.join("alternate"),
        }
    }

    fn target_path(&self, target: &BoundTarget) -> PathBuf {
        let mut components = target
            .target
            .relative
            .as_str()
            .split('/')
            .collect::<Vec<_>>();
        components.pop();
        let mut path = self.locator.join(self.layout.data);
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

    fn resolved(
        &self,
        target: &BoundTarget,
    ) -> Result<marduk_platform::ResolvedTarget<'_>, RootError> {
        self.binding.verify()?;
        let resolved =
            marduk_platform::resolve_existing_parent(&self.binding, &target.target.relative)?;
        if resolved.parent_identity() != &target.parent || resolved.leaf() != target.leaf {
            return Err(RootError::OutsideBoundary);
        }
        Ok(resolved)
    }

    fn observe_bound(&self, target: &BoundTarget) -> Observation {
        match self.resolved(target) {
            Ok(resolved) => match marduk_platform::observe_target(&resolved) {
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

    /// Observe the target and both operation-owned slots. An exchange leaves
    /// the actually displaced object in the stage slot, so the slot's contents
    /// distinguish "stage untouched" from "displaced target retained".
    fn evidence_for(
        &self,
        target: &BoundTarget,
        op: OperationId,
        ordinal: u32,
        direction: StepDirection,
        exchange: bool,
        staged: Option<&Fingerprint>,
    ) -> Evidence {
        let directory = self.op_dir(op.get(), ordinal, direction);
        let stage_observation = artifact(&directory.join("stage"));
        let (stage, retained) = if exchange {
            match &stage_observation {
                Artifact::Regular(actual) if staged == Some(actual) => {
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

    fn evidence_for_step(&self, step: &PreparedStep) -> Evidence {
        let exchange = matches!(step.spec.pre, PreState::Regular(_)) && step.staged.is_some();
        self.evidence_for(
            &step.spec.target,
            step.op,
            step.ordinal,
            step.direction,
            exchange,
            step.staged.as_ref().map(|(_, fingerprint)| fingerprint),
        )
    }

    fn validate_backup_inner(
        &self,
        at: BackupRef,
        expected: &Fingerprint,
    ) -> Result<(), DeployError> {
        match effects::fingerprint_path(&self.backup_path(at)) {
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

    /// Copy `source` to `destination` once, then prove the durable artifact's
    /// shape. An existing operation-owned artifact is reused only when it
    /// already matches; it is never overwritten.
    fn ensure_copy(
        &self,
        source: &Path,
        destination: &Path,
        len: u64,
        sha256: [u8; 32],
        metadata: Option<&FileMetadata>,
    ) -> Result<Fingerprint, DeployError> {
        let existing = match artifact(destination) {
            Artifact::Absent => {
                effects::copy_file(source, destination).map_err(copy_error)?;
                artifact(destination)
            }
            other => other,
        };
        match existing {
            Artifact::Regular(fingerprint)
                if fingerprint.len == len
                    && fingerprint.sha256 == sha256
                    && metadata.is_none_or(|expected| fingerprint.metadata == *expected) =>
            {
                Ok(fingerprint)
            }
            Artifact::Regular(_) | Artifact::Unsupported(_) => Err(DeployError::RecoveryConflict),
            Artifact::Absent | Artifact::Unavailable => Err(DeployError::Io),
        }
    }

    fn publish_absent(&mut self, target: &BoundTarget, staged: StageRef) -> EffectOutcome {
        let key = (
            staged.operation().get(),
            staged.ordinal(),
            staged.direction(),
        );
        let Some(staged_fingerprint) = self.staged.get(&key).cloned() else {
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
            return EffectOutcome::Conflict(self.evidence_for(
                target,
                staged.operation(),
                staged.ordinal(),
                staged.direction(),
                false,
                Some(&staged_fingerprint),
            ));
        }

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
            let evidence = self.evidence_for(
                target,
                staged.operation(),
                staged.ordinal(),
                staged.direction(),
                false,
                Some(&staged_fingerprint),
            );
            return match observed_after {
                Observation::Unavailable(error) => EffectOutcome::Unavailable(error),
                Observation::Absent => EffectOutcome::Unavailable(RootError::Io),
                _ => EffectOutcome::Conflict(evidence),
            };
        }
        let Ok(stage_parent) = effects::directory_identity(&stage_directory) else {
            return EffectOutcome::Unavailable(RootError::TargetUnavailable);
        };
        if !same_rename_domain(&stage_parent, &target_parent_identity) {
            return EffectOutcome::Unavailable(RootError::OutsideBoundary);
        }
        let stage_dir = match effects::open_directory(&stage_directory) {
            Ok(directory) => directory,
            Err(error) => return EffectOutcome::Unavailable(error),
        };

        let result = effects::publish(
            &stage_dir,
            "stage",
            &target_directory,
            &leaf,
            &stage_path,
            &target_path,
        );
        let evidence = self.evidence_for(
            target,
            staged.operation(),
            staged.ordinal(),
            staged.direction(),
            false,
            Some(&staged_fingerprint),
        );

        if result.is_err() {
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
        if evidence.target == Observation::Regular(staged_fingerprint)
            && evidence.stage == Artifact::Absent
            && evidence.retained == Artifact::Absent
        {
            EffectOutcome::Observed(evidence)
        } else {
            EffectOutcome::Ambiguous(evidence)
        }
    }

    fn exchange_target(
        &mut self,
        target: &BoundTarget,
        expected: &Fingerprint,
        staged: StageRef,
        retain: BackupRef,
    ) -> EffectOutcome {
        let key = (
            staged.operation().get(),
            staged.ordinal(),
            staged.direction(),
        );
        let Some(staged_fingerprint) = self.staged.get(&key).cloned() else {
            return EffectOutcome::Unavailable(RootError::TargetUnavailable);
        };
        if retain
            != BackupRef::from_record(
                staged.operation(),
                staged.ordinal(),
                staged.direction(),
                BackupSlot::Displaced,
            )
        {
            return EffectOutcome::Conflict(self.evidence_for(
                target,
                staged.operation(),
                staged.ordinal(),
                staged.direction(),
                true,
                Some(&staged_fingerprint),
            ));
        }
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
            return EffectOutcome::Conflict(self.evidence_for(
                target,
                staged.operation(),
                staged.ordinal(),
                staged.direction(),
                true,
                Some(&staged_fingerprint),
            ));
        }

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
            return EffectOutcome::Conflict(self.evidence_for(
                target,
                staged.operation(),
                staged.ordinal(),
                staged.direction(),
                true,
                Some(&staged_fingerprint),
            ));
        }
        let Ok(stage_parent) = effects::directory_identity(&stage_directory) else {
            return EffectOutcome::Unavailable(RootError::TargetUnavailable);
        };
        if !same_rename_domain(&stage_parent, &target_parent_identity) {
            return EffectOutcome::Unavailable(RootError::OutsideBoundary);
        }
        let stage_dir = match effects::open_directory(&stage_directory) {
            Ok(directory) => directory,
            Err(error) => return EffectOutcome::Unavailable(error),
        };

        // The exchange alias puts the actually displaced file in the stage
        // slot; that slot is the canonical retain evidence.
        let result = effects::replace(
            &stage_dir,
            "stage",
            &target_directory,
            &leaf,
            &stage_path,
            &target_path,
            &stage_path,
        );
        let evidence = self.evidence_for(
            target,
            staged.operation(),
            staged.ordinal(),
            staged.direction(),
            true,
            Some(&staged_fingerprint),
        );

        if let Err(error) = result {
            if matches!(error, RootError::UnsupportedTarget(_)) {
                return EffectOutcome::Unavailable(error);
            }
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

    fn detach(
        &mut self,
        target: &BoundTarget,
        expected: &Fingerprint,
        retain: BackupRef,
    ) -> EffectOutcome {
        if retain.slot() != BackupSlot::Displaced {
            return EffectOutcome::Conflict(self.evidence_for(
                target,
                retain.operation(),
                retain.ordinal(),
                retain.direction(),
                false,
                None,
            ));
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
            return EffectOutcome::Conflict(self.evidence_for(
                target,
                retain.operation(),
                retain.ordinal(),
                retain.direction(),
                false,
                None,
            ));
        }

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
            return EffectOutcome::Conflict(self.evidence_for(
                target,
                retain.operation(),
                retain.ordinal(),
                retain.direction(),
                false,
                None,
            ));
        }
        let Ok(displaced_parent) = effects::directory_identity(&displaced_directory) else {
            return EffectOutcome::Unavailable(RootError::TargetUnavailable);
        };
        if !same_rename_domain(&target_parent_identity, &displaced_parent) {
            return EffectOutcome::Unavailable(RootError::OutsideBoundary);
        }
        let displaced_dir = match effects::open_directory(&displaced_directory) {
            Ok(directory) => directory,
            Err(error) => return EffectOutcome::Unavailable(error),
        };

        let result = effects::detach(
            &target_directory,
            &leaf,
            &displaced_dir,
            "displaced",
            &target_path,
            &displaced_path,
        );
        let evidence = self.evidence_for(
            target,
            retain.operation(),
            retain.ordinal(),
            retain.direction(),
            false,
            None,
        );

        if result.is_err() {
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
        if evidence.target == Observation::Absent
            && evidence.stage == Artifact::Absent
            && evidence.retained == Artifact::Regular(expected.clone())
        {
            EffectOutcome::Observed(evidence)
        } else {
            EffectOutcome::Ambiguous(evidence)
        }
    }

    /// Flush an operation directory and every ancestor up to the app data root.
    /// A new entry is only durable once its parent directory is.
    fn sync_directory_chain(&self, directory: &Path) -> Result<(), RootError> {
        effects::sync_directory(directory)?;
        let mut current = directory.parent();
        while let Some(path) = current {
            effects::sync_directory(path)?;
            if path == self.library {
                break;
            }
            current = path.parent();
        }
        Ok(())
    }

    fn sync_effect(&self, step: &PreparedStep) -> Result<(), RootError> {
        let directory = self.op_dir(step.op.get(), step.ordinal, step.direction);
        for name in ["stage", "displaced", "original", "step"] {
            let path = directory.join(name);
            if fs::symlink_metadata(&path).is_ok() {
                let file = effects::open_file(&path)?;
                file.sync_all().map_err(|_| RootError::Io)?;
            }
        }
        if let Observation::Regular(_) = self.observe_bound(&step.spec.target) {
            let file = effects::open_file(&self.target_path(&step.spec.target))?;
            file.sync_all().map_err(|_| RootError::Io)?;
        }
        self.sync_directory_chain(&directory)?;
        // The changed target parent is part of the barrier. A failed
        // re-resolution or open must not leave the effect reported durable.
        let resolved = self.resolved(&step.spec.target)?;
        let target_directory = resolved.parent_dir()?;
        target_directory.sync_all().map_err(|_| RootError::Io)?;
        Ok(())
    }
}

impl CopyEffects for NativeEffects {
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
        fs::create_dir_all(self.op_dir(op.get(), ordinal, direction))
            .map_err(|_| DeployError::Io)?;

        let resolved = self.resolved(&intent.target).map_err(map_root)?;
        let observed = marduk_platform::observe_target(&resolved).map_err(map_root)?;
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
                self.ensure_copy(
                    &target_path,
                    &self.backup_path(at),
                    expected.len,
                    expected.sha256,
                    Some(&expected.metadata),
                )?;
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
                self.ensure_copy(
                    &target_path,
                    &self.backup_path(at),
                    fingerprint.len,
                    fingerprint.sha256,
                    Some(&fingerprint.metadata),
                )?;
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
                let observed =
                    effects::fingerprint_path(&source_path).map_err(|_| DeployError::StalePlan)?;
                if observed.len != source.len || observed.sha256 != source.sha256 {
                    return Err(DeployError::StalePlan);
                }
                let stage_path = self.stage_path(StageRef::from_record(op, ordinal, direction));
                let fingerprint =
                    self.ensure_copy(&source_path, &stage_path, source.len, source.sha256, None)?;
                Some(fingerprint)
            }
            StepOutput::Original(at) => {
                let PreState::Regular(expected) = &intent.original.observation else {
                    return Err(DeployError::RecoveryConflict);
                };
                self.validate_backup_inner(*at, expected)?;
                let stage_path = self.stage_path(StageRef::from_record(op, ordinal, direction));
                let fingerprint = self.ensure_copy(
                    &self.backup_path(*at),
                    &stage_path,
                    expected.len,
                    expected.sha256,
                    Some(&expected.metadata),
                )?;
                Some(fingerprint)
            }
        };

        let displaced = self.op_dir(op.get(), ordinal, direction).join("displaced");
        if artifact(&displaced) != Artifact::Absent {
            return Err(DeployError::RecoveryConflict);
        }
        if staged.is_some() {
            let stage_directory = self.op_dir(op.get(), ordinal, direction);
            let target_parent = self
                .resolved(&intent.target)
                .map_err(map_root)?
                .parent_identity()
                .clone();
            let stage_parent = effects::directory_identity(&stage_directory).map_err(map_root)?;
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
        if let Some((_, fingerprint)) = &prepared.staged {
            self.staged
                .insert((op.get(), ordinal, direction), fingerprint.clone());
        }
        // The stage and backups must be durable before the caller may publish.
        // The operation chain is synced here, not only after the public rename.
        self.sync_directory_chain(&self.op_dir(op.get(), ordinal, direction))
            .map_err(|error| match error {
                RootError::UnsupportedTarget(_) => DeployError::UnsupportedTarget,
                _ => DeployError::Io,
            })?;
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
        // Cache the sealed fingerprint on every inspection. Recovery calls this
        // immediately before execute/execute_undo, so a reopened adapter can
        // still classify an exchange without having prepared the step itself.
        match &step.staged {
            Some((at, fingerprint)) => {
                self.staged.insert(
                    (at.operation().get(), at.ordinal(), at.direction()),
                    fingerprint.clone(),
                );
            }
            None => {
                self.staged
                    .remove(&(step.op.get(), step.ordinal, step.direction));
            }
        }
        self.evidence_for_step(step)
    }

    fn inspect_alternate_displacement(&mut self, step: &PreparedStep) -> Artifact {
        artifact(&self.backup_path(step.alternate_displacement()))
    }

    fn durability_barrier(&mut self, step: &PreparedStep, observed: &Evidence) -> BarrierOutcome {
        let current = self.evidence_for_step(step);
        if current != *observed {
            return BarrierOutcome::Ambiguous(current);
        }
        if cfg!(windows) {
            // No documented non-admin Windows namespace barrier is qualified.
            return BarrierOutcome::Unavailable(RootError::UnsupportedTarget(
                UnsupportedReason::Configuration,
            ));
        }
        if let Err(error) = self.sync_effect(step) {
            return BarrierOutcome::Unavailable(error);
        }
        let after = self.evidence_for_step(step);
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

impl DeploymentInputs for NativeEffects {
    type RootGuard = WriterGuard;
    type LibraryGuard = WriterGuard;

    fn try_root_gate(&mut self) -> Result<Self::RootGuard, DeployError> {
        self.root_gate.try_lock().map_err(gate_error)
    }

    fn try_library_gate(&mut self) -> Result<Self::LibraryGuard, DeployError> {
        self.library_gate.try_lock().map_err(gate_error)
    }

    fn verify_root(
        &mut self,
        installation: &RootIdentity,
        data: &RootIdentity,
    ) -> Result<(), DeployError> {
        // Reopen the recorded locator rather than trusting the handles from
        // composition. A substituted, moved or mismatched pair is refused; the
        // adapter never adopts the replacement.
        let binding =
            marduk_platform::reopen_installation(&self.locator, &self.layout, installation, data)
                .map_err(map_root)?;
        self.binding = binding;
        Ok(())
    }

    fn verify_recovery_root(
        &mut self,
        installation: &RootIdentity,
        data: &RootIdentity,
        locator: Option<&[u8]>,
    ) -> Result<(), DeployError> {
        let recorded = locator
            .and_then(crate::operation_journal::decode_locator)
            .ok_or(DeployError::TargetUnavailable)?;
        // The adapter must have been composed from the same recorded locator;
        // a different spelling is a typed refusal, never a silent re-bind.
        if recorded != self.locator {
            return Err(DeployError::TargetUnavailable);
        }
        let binding =
            marduk_platform::reopen_installation(&recorded, &self.layout, installation, data)
                .map_err(map_root)?;
        self.binding = binding;
        Ok(())
    }

    fn resolve_target(&mut self, target: &TargetFile) -> Result<BoundTarget, RootError> {
        self.binding.verify()?;
        let resolved = marduk_platform::resolve_existing_parent(&self.binding, &target.relative)?;
        Ok(BoundTarget {
            target: target.clone(),
            parent: resolved.parent_identity().clone(),
            leaf: resolved.leaf().to_owned(),
        })
    }

    fn profile_revision(&mut self, profile: ProfileId) -> Result<ProfileRevision, DeployError> {
        let path =
            marduk_platform::private_child_path(&self.root_handle, crate::schema::CATALOG_FILE)
                .map_err(|_| DeployError::Io)?;
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|_| DeployError::Io)?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(|_| DeployError::Io)?;
        crate::library::read_profile_revision(&connection, profile).map_err(|_| DeployError::Io)
    }

    fn source(
        &mut self,
        installed: InstalledRevisionId,
        path: &RelativePath,
    ) -> Result<SourceRef, DeployError> {
        let fingerprint =
            effects::fingerprint_path(&self.source_path(installed, path)).map_err(|error| {
                match error {
                    RootError::TargetUnavailable => DeployError::StalePlan,
                    RootError::UnsupportedTarget(_) => DeployError::UnsupportedTarget,
                    RootError::Io => DeployError::Io,
                    _ => DeployError::InvalidPath,
                }
            })?;
        Ok(SourceRef {
            installed,
            path: path.clone(),
            sha256: fingerprint.sha256,
            len: fingerprint.len,
        })
    }
}

fn artifact(path: &Path) -> Artifact {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Artifact::Unsupported(UnsupportedReason::ObjectKind)
        }
        Ok(_) => match effects::fingerprint_path(path) {
            Ok(fingerprint) => Artifact::Regular(fingerprint),
            Err(RootError::UnsupportedTarget(reason)) => Artifact::Unsupported(reason),
            // The stat succeeded, so "absent" is no longer a possible answer:
            // an unreadable slot is unavailable evidence, never an empty one.
            Err(_) => Artifact::Unavailable,
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => Artifact::Absent,
        Err(_) => Artifact::Unavailable,
    }
}

fn same_rename_domain(left: &RootIdentity, right: &RootIdentity) -> bool {
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

fn copy_error(error: RootError) -> DeployError {
    match error {
        RootError::TargetUnavailable => DeployError::TargetUnavailable,
        RootError::UnsupportedTarget(_) => DeployError::UnsupportedTarget,
        _ => DeployError::Io,
    }
}

fn gate_error(error: GateError) -> DeployError {
    match error {
        GateError::Busy => DeployError::Busy,
        _ => DeployError::TargetUnavailable,
    }
}
