include!("recovery.rs");

#[test]
fn replace_undo_published_without_ack_is_not_repeated() {
    let mut f = Fixture::new(WINNER);
    let plan = f.plan();
    let op = f.cut_apply(plan, Cut::BeforeWinnerAck);
    f.boot();
    f.journal
        .set_phase(op, Phase::Applying, Phase::RollingBack)
        .unwrap();
    let forward = f.journal.durable.steps[&op][1].prepared.clone();
    let _guard = f.effects.try_root_gate().unwrap();
    let spec = StepSpec {
        target: forward.spec.target.clone(),
        original: forward.spec.original.clone(),
        pre: PreState::Regular(forward.staged.as_ref().unwrap().1.clone()),
        output: StepOutput::Original(forward.step_pre.backup.unwrap()),
    };
    let undo = f
        .effects
        .prepare(op, forward.ordinal, StepDirection::Undo, &spec)
        .unwrap();
    f.journal
        .record_undo(op, forward.ordinal, undo.clone())
        .unwrap();
    f.journal
        .record_step(
            op,
            forward.ordinal,
            StepPhase::Attempting,
            StepPhase::UndoAttempting,
            None,
        )
        .unwrap();
    let PreState::Regular(expected) = &undo.spec.pre else {
        unreachable!()
    };
    let outcome = f.effects.replace_existing_retain_displaced(
        &undo.spec.target,
        expected,
        undo.staged.as_ref().unwrap().0,
        undo.retain,
    );
    let EffectOutcome::Displaced { evidence, .. } = outcome else {
        panic!("replacement did not publish")
    };
    assert!(matches!(
        f.effects.durability_barrier(&undo, &evidence),
        BarrierOutcome::Durable(_)
    ));
    drop(_guard);
    // Restart after the durable replacement but before recording Undone.
    let row = &f.journal.durable.steps[&op][1];
    assert_eq!(row.phase, StepPhase::UndoAttempting);
    let published = &f.effects.durable.public[WINNER].fingerprint;
    assert_ne!(row.prepared.spec.pre, PreState::Regular(published.clone()));
    assert_eq!(
        &row.undo.as_ref().unwrap().staged.as_ref().unwrap().1,
        published
    );
    f.boot();
    f.assert_resolved(op, OperationKind::Deploy, Phase::RolledBack, None);
    f.mutations(&[
        (Mutation::Create, CREATED),
        (Mutation::Replace, WINNER),
        (Mutation::Replace, WINNER),
        (Mutation::Detach, CREATED),
    ]);
}

#[test]
fn restore_rechecks_an_earlier_target_after_the_last_effect() {
    let mut f = Fixture::new(WINNER);
    let deploy = f.deploy();
    let op = f.cut_restore();
    f.boot();
    let mut port = EditAfterDetach(&mut f.effects);
    let status = recover_pending(&mut f.journal, &mut port, &root(1, 1), &root(1, 2))
        .unwrap()
        .unwrap();
    assert_eq!(status.blocker, Some(DeployError::RecoveryConflict));
    f.root_state(Some(op), deploy.generation);
    f.files(None, EDIT);
    assert_eq!(f.effects.mutations.len(), 4);
    assert_eq!(
        f.recover().unwrap().blocker,
        Some(DeployError::RecoveryConflict)
    );
    assert_eq!(f.effects.mutations.len(), 4);
}

struct EditAfterDetach<'a>(&'a mut Effects);

impl DeploymentInputs for EditAfterDetach<'_> {
    type RootGuard = Gate;
    type LibraryGuard = ();

    fn try_root_gate(&mut self) -> Result<Gate, DeployError> {
        self.0.try_root_gate()
    }
    fn try_library_gate(&mut self) -> Result<(), DeployError> {
        unreachable!()
    }
    fn verify_root(
        &mut self,
        installation: &RootIdentity,
        data: &RootIdentity,
    ) -> Result<(), DeployError> {
        self.0.verify_root(installation, data)
    }
    fn resolve_target(&mut self, _: &TargetFile) -> Result<BoundTarget, RootError> {
        unreachable!()
    }
    fn profile_revision(&mut self, _: ProfileId) -> Result<ProfileRevision, DeployError> {
        unreachable!()
    }
    fn source(
        &mut self,
        _: InstalledRevisionId,
        _: &RelativePath,
    ) -> Result<SourceRef, DeployError> {
        unreachable!()
    }
}

impl CopyEffects for EditAfterDetach<'_> {
    fn observe(&mut self, target: &BoundTarget) -> Observation {
        self.0.observe(target)
    }
    fn prepare(
        &mut self,
        _: OperationId,
        _: u32,
        _: StepDirection,
        _: &StepSpec,
    ) -> Result<PreparedStep, DeployError> {
        unreachable!()
    }
    fn publish_new_absent(&mut self, _: &BoundTarget, _: StageRef) -> EffectOutcome {
        unreachable!()
    }
    fn replace_existing_retain_displaced(
        &mut self,
        _: &BoundTarget,
        _: &Fingerprint,
        _: StageRef,
        _: BackupRef,
    ) -> EffectOutcome {
        unreachable!()
    }
    fn detach_existing_retain(
        &mut self,
        target: &BoundTarget,
        expected: &Fingerprint,
        retain: BackupRef,
    ) -> EffectOutcome {
        let outcome = self.0.detach_existing_retain(target, expected, retain);
        self.0.external_write(WINNER, EDIT);
        outcome
    }
    fn inspect_step(&mut self, step: &PreparedStep) -> Evidence {
        self.0.inspect_step(step)
    }
    fn durability_barrier(&mut self, step: &PreparedStep, evidence: &Evidence) -> BarrierOutcome {
        self.0.durability_barrier(step, evidence)
    }
    fn validate_backup(
        &mut self,
        at: BackupRef,
        expected: &Fingerprint,
    ) -> Result<(), DeployError> {
        self.0.validate_backup(at, expected)
    }
}

#[test]
fn matching_pre_target_does_not_excuse_missing_stage() {
    let mut f = Fixture::new(WINNER);
    let plan = f.plan();
    let op = f.cut_apply(plan, Cut::BeforeFirstPublish);
    let stage = f.journal.durable.steps[&op][1]
        .prepared
        .staged
        .as_ref()
        .unwrap()
        .0;
    assert!(take(&mut f.effects.durable.staged, stage).is_some());
    f.boot();
    let status = f.recover().unwrap();
    assert_eq!(status.phase, Phase::RollingBack);
    assert_eq!(status.blocker, Some(DeployError::RecoveryConflict));
    f.root_state(Some(op), None);
    f.mutations(&[]);
}

#[test]
fn matching_post_target_without_displacement_is_not_undone() {
    let mut f = Fixture::new(WINNER);
    let plan = f.plan();
    let op = f.cut_apply(plan, Cut::BeforeWinnerAck);
    let retain = f.journal.durable.steps[&op][1].prepared.retain;
    assert!(take(&mut f.effects.durable.backups, retain).is_some());
    f.boot();
    assert_eq!(
        f.recover().unwrap().blocker,
        Some(DeployError::RecoveryConflict)
    );
    f.root_state(Some(op), None);
    f.files(Some(ONLY_A), B);
    f.mutations(&[(Mutation::Create, CREATED), (Mutation::Replace, WINNER)]);
}

#[test]
fn restore_does_not_retry_without_the_sealed_step_backup() {
    let mut f = Fixture::new(WINNER);
    let deploy = f.deploy();
    let op = f.cut_restore();
    let backup = f.journal.durable.steps[&op]
        .iter()
        .find(|row| row.prepared.ordinal == 0)
        .unwrap()
        .prepared
        .step_pre
        .backup
        .unwrap();
    assert!(take(&mut f.effects.durable.backups, backup).is_some());
    f.boot();
    assert_eq!(
        f.recover().unwrap().blocker,
        Some(DeployError::RecoveryConflict)
    );
    f.root_state(Some(op), deploy.generation);
    f.files(Some(ONLY_A), BASE);
    assert_eq!(f.effects.mutations.len(), 3);
}

#[test]
fn restore_continuation_rejects_a_later_missing_step_backup_before_any_effect() {
    let mut f = Fixture::new(WINNER);
    let deploy = f.deploy();
    let op = f.cut_restore();
    let rows = &f.journal.durable.steps[&op];
    let earlier = rows
        .iter()
        .find(|row| row.prepared.spec.target.target.relative.as_str() == CREATED)
        .unwrap();
    let later = rows
        .iter()
        .find(|row| row.prepared.spec.target.target.relative.as_str() == WINNER)
        .unwrap();
    // The created row runs first in the reverse order; the winner row is the
    // later conflict and its sealed pre-image is the one that goes missing.
    assert_eq!(earlier.prepared.ordinal, 0);
    assert_eq!(later.prepared.ordinal, 1);
    let earlier_backup = earlier.prepared.step_pre.backup.unwrap();
    let later_backup = later.prepared.step_pre.backup.unwrap();
    assert!(lookup(&f.effects.durable.backups, earlier_backup).is_some());
    let sealed = take(&mut f.effects.durable.backups, later_backup).unwrap();
    let effects = f.effects.mutations.len();
    let finishes = f.journal.finishes;
    f.boot();
    for _ in 0..2 {
        let status = f.recover().unwrap();
        assert_eq!(
            (status.kind, status.phase, status.generation),
            (OperationKind::Restore, Phase::Restoring, deploy.generation)
        );
        assert_eq!(status.blocker, Some(DeployError::RecoveryConflict));
        f.assert_status(
            &status,
            OperationKind::Restore,
            Phase::Restoring,
            Some(DeployError::RecoveryConflict),
        );
        f.root_state(Some(op), deploy.generation);
        // The earlier readable row must not have been detached first.
        f.files(Some(ONLY_A), BASE);
        assert_eq!(f.effects.mutations.len(), effects);
        assert_eq!(f.journal.finishes, finishes);
    }
    // Restoring the sealed pre-image makes the same continuation runnable.
    f.effects
        .visible
        .backups
        .push((later_backup, sealed.clone()));
    f.effects.durable.backups.push((later_backup, sealed));
    let status = f.recover().unwrap();
    f.assert_status(&status, OperationKind::Restore, Phase::Completed, None);
    f.root_state(None, None);
    f.files(None, BASE);
    f.mutations(&[
        (Mutation::Create, CREATED),
        (Mutation::Replace, WINNER),
        (Mutation::Replace, WINNER),
        (Mutation::Detach, CREATED),
    ]);
}

#[test]
fn deploy_rollback_classifies_a_later_conflict_before_any_effect() {
    let mut f = Fixture::new(WINNER);
    let plan = f.plan();
    let op = f.cut_apply(plan, Cut::BeforeWinnerAck);
    // The winner row is resolvable and rolls back first in the reverse order;
    // the created row is externally edited and conflicts second.
    f.effects.external_write(CREATED, ONLY_A);
    let external = f.effects.durable.public[CREATED].clone();
    let effects = f.effects.mutations.len();
    f.boot();
    for _ in 0..2 {
        let status = f.recover().unwrap();
        assert_eq!(
            (status.kind, status.phase, status.generation),
            (OperationKind::Deploy, Phase::RollingBack, None)
        );
        assert_eq!(status.blocker, Some(DeployError::RecoveryConflict));
        assert_eq!(f.state(op).blocker, status.blocker);
        f.root_state(Some(op), None);
        assert_eq!(f.effects.durable.public[CREATED], external);
        assert_eq!(f.effects.durable.public[WINNER].bytes, B);
        assert_eq!(f.effects.mutations.len(), effects);
        assert_eq!(f.journal.finishes, 0);
    }
}

#[test]
fn restore_continuation_classifies_a_later_conflict_before_any_effect() {
    let mut f = Fixture::new(WINNER);
    let deploy = f.deploy();
    let op = f.cut_restore();
    // The created row is resolvable and restores first in the reverse order;
    // the winner row was externally edited and conflicts second.
    f.effects.external_write(WINNER, EDIT);
    let external = f.effects.durable.public[WINNER].clone();
    let effects = f.effects.mutations.len();
    let finishes = f.journal.finishes;
    f.boot();
    for _ in 0..2 {
        let status = f.recover().unwrap();
        assert_eq!(
            (status.kind, status.phase, status.generation),
            (OperationKind::Restore, Phase::Restoring, deploy.generation)
        );
        assert_eq!(status.blocker, Some(DeployError::RecoveryConflict));
        f.assert_status(
            &status,
            OperationKind::Restore,
            Phase::Restoring,
            Some(DeployError::RecoveryConflict),
        );
        f.root_state(Some(op), deploy.generation);
        assert_eq!(f.effects.durable.public[WINNER], external);
        assert_eq!(f.effects.durable.public[CREATED].bytes, ONLY_A);
        assert_eq!(f.effects.mutations.len(), effects);
        assert_eq!(f.journal.finishes, finishes);
    }
}
