//! Stage-change gate regression kept outside the frozen recovery oracle.
//!
//! `recovery.rs` is a frozen acceptance witness and must not be extended. This
//! target reuses its fixture read-only and injects one stage mutation through a
//! wrapper port, so the regression is exercised without editing that oracle.

include!("recovery.rs");

struct StageChangedAfterPreflight<'a> {
    inner: &'a mut Effects,
    reads: usize,
    corrupt_at: usize,
}

impl<'a> StageChangedAfterPreflight<'a> {
    fn new(inner: &'a mut Effects, corrupt_at: usize) -> Self {
        Self {
            inner,
            reads: 0,
            corrupt_at,
        }
    }
}

impl DeploymentInputs for StageChangedAfterPreflight<'_> {
    type RootGuard = <Effects as DeploymentInputs>::RootGuard;
    type LibraryGuard = <Effects as DeploymentInputs>::LibraryGuard;

    fn try_root_gate(&mut self) -> Result<Self::RootGuard, DeployError> {
        self.inner.try_root_gate()
    }

    fn try_library_gate(&mut self) -> Result<Self::LibraryGuard, DeployError> {
        self.inner.try_library_gate()
    }

    fn verify_root(
        &mut self,
        installation: &RootIdentity,
        data: &RootIdentity,
    ) -> Result<(), DeployError> {
        self.inner.verify_root(installation, data)
    }

    fn verify_recovery_root(
        &mut self,
        installation: &RootIdentity,
        data: &RootIdentity,
        locator: Option<&[u8]>,
    ) -> Result<(), DeployError> {
        self.inner.verify_recovery_root(installation, data, locator)
    }

    fn resolve_target(&mut self, target: &TargetFile) -> Result<BoundTarget, RootError> {
        self.inner.resolve_target(target)
    }

    fn profile_revision(&mut self, profile: ProfileId) -> Result<ProfileRevision, DeployError> {
        self.inner.profile_revision(profile)
    }

    fn source(
        &mut self,
        installed: InstalledRevisionId,
        path: &RelativePath,
    ) -> Result<SourceRef, DeployError> {
        self.inner.source(installed, path)
    }
}

impl CopyEffects for StageChangedAfterPreflight<'_> {
    fn observe(&mut self, target: &BoundTarget) -> Observation {
        self.inner.observe(target)
    }

    fn prepare(
        &mut self,
        op: OperationId,
        ordinal: u32,
        direction: StepDirection,
        intent: &StepSpec,
    ) -> Result<PreparedStep, DeployError> {
        self.inner.prepare(op, ordinal, direction, intent)
    }

    fn publish_new_absent(&mut self, target: &BoundTarget, staged: StageRef) -> EffectOutcome {
        self.inner.publish_new_absent(target, staged)
    }

    fn replace_existing_retain_displaced(
        &mut self,
        target: &BoundTarget,
        expected: &Fingerprint,
        staged: StageRef,
        retain: BackupRef,
    ) -> EffectOutcome {
        self.inner
            .replace_existing_retain_displaced(target, expected, staged, retain)
    }

    fn detach_existing_retain(
        &mut self,
        target: &BoundTarget,
        expected: &Fingerprint,
        retain: BackupRef,
    ) -> EffectOutcome {
        self.inner.detach_existing_retain(target, expected, retain)
    }

    fn inspect_step(&mut self, step: &PreparedStep) -> Evidence {
        if step.ordinal == 0 {
            self.reads += 1;
            if self.reads >= self.corrupt_at
                && let Some((at, _)) = step.staged
            {
                self.inner.visible.staged.retain(|(slot, _)| *slot != at);
            }
        }
        self.inner.inspect_step(step)
    }

    fn inspect_alternate_displacement(&mut self, step: &PreparedStep) -> Artifact {
        self.inner.inspect_alternate_displacement(step)
    }

    fn durability_barrier(&mut self, step: &PreparedStep, observed: &Evidence) -> BarrierOutcome {
        self.inner.durability_barrier(step, observed)
    }

    fn validate_backup(
        &mut self,
        at: BackupRef,
        expected: &Fingerprint,
    ) -> Result<(), DeployError> {
        self.inner.validate_backup(at, expected)
    }
}

#[test]
fn a_stage_changed_after_preflight_blocks_before_any_effect() {
    let mut fixture = Fixture::new(WINNER);
    let plan = fixture.plan();
    // First inspection is the operation-wide preflight; the second is the
    // per-step apply boundary for ordinal 0 (the created file).
    let mut port = StageChangedAfterPreflight::new(&mut fixture.effects, 2);

    assert_eq!(
        commit_deployment(&mut fixture.journal, &mut port, plan, || false),
        Err(DeployError::RecoveryConflict)
    );
    assert!(
        fixture.effects.mutations.is_empty(),
        "no later effect may follow a changed stage"
    );
    let pending = fixture
        .journal
        .visible
        .pending
        .expect("the blocked operation stays pending");
    let status = fixture.state(pending);
    assert_eq!(
        (status.phase, status.blocker),
        (Phase::Applying, Some(DeployError::RecoveryConflict))
    );
    assert_eq!(fixture.effects.visible.public[WINNER].bytes, BASE);
    assert!(!fixture.effects.visible.public.contains_key(CREATED));
}
