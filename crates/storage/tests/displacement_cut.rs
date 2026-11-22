include!("recovery_resume.rs");

#[test]
fn recovery_reports_external_edit_retained_before_effect_ack() {
    assert_displacement_cut(false);
}

#[test]
fn recovery_checks_the_alternate_even_with_a_settled_canonical_slot() {
    assert_displacement_cut(true);
}

fn assert_displacement_cut(alternate: bool) {
    let dir = fixture_v1();
    let mut journal = open_fixture(&dir);
    let mut effects = MemEffects::new();
    let plan = save_plan(&mut journal);
    let op = journal.claim(Claim::Deploy(plan)).unwrap();
    let step = deploy_step(op);
    journal.seal(op, vec![step.clone()]).unwrap();
    journal
        .set_phase(op, Phase::Prepared, Phase::Applying)
        .unwrap();
    journal
        .record_step(op, 0, StepPhase::Ready, StepPhase::Attempting, None)
        .unwrap();

    // Fake effect survived; its return value and SQL update did not. The
    // external edit raced the checked exchange, not its later inspection.
    effects.public.insert(
        FILE.to_owned(),
        MemFile {
            bytes: POST.to_vec(),
            fingerprint: fingerprint(POST, 0),
        },
    );
    let displaced = MemFile {
        bytes: EDIT.to_vec(),
        fingerprint: fingerprint(EDIT, 42),
    };
    let at = if alternate {
        effects.backups.insert(
            step.retain,
            MemFile {
                bytes: BASE.to_vec(),
                fingerprint: fingerprint(BASE, 10),
            },
        );
        step.alternate_displacement()
    } else {
        step.retain
    };
    effects.backups.insert(at, displaced.clone());
    drop(journal);
    let mut journal = OperationJournal::open(dir.path()).unwrap();
    recover_pending(&mut journal, &mut effects, &root(1), &root(2))
        .unwrap()
        .unwrap();
    journal.reopen().unwrap();
    let status = journal.status(op).unwrap().unwrap();
    assert_eq!(status.blocker, Some(DeployError::RecoveryConflict));
    assert_eq!(status.generation, None);
    assert!(status.retained.contains(&at));
    let evidence = journal.unexpected_displacements(op).unwrap();
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].0, at);
    assert_eq!(
        evidence[0].1.retained,
        Artifact::Regular(displaced.fingerprint.clone())
    );
    assert_eq!(effects.backups[&at], displaced);
    assert_eq!(
        recover_pending(&mut journal, &mut effects, &root(1), &root(2))
            .unwrap()
            .unwrap()
            .retained,
        status.retained
    );
    assert_eq!(effects.mutations, 0);
    assert_eq!(
        effects.public[FILE].bytes, POST,
        "a blocked inspection must not publish"
    );
    assert_eq!(
        journal.root_state(&root(2)).unwrap(),
        RootState {
            pending: Some(op),
            active: None
        }
    );
}

#[test]
fn unreadable_canonical_retain_blocks_recovery_without_effect() {
    let dir = fixture_v1();
    let mut journal = open_fixture(&dir);
    let mut effects = MemEffects::new();
    let plan = save_plan(&mut journal);
    let op = journal.claim(Claim::Deploy(plan)).unwrap();
    let step = deploy_step(op);
    journal.seal(op, vec![step.clone()]).unwrap();
    journal
        .set_phase(op, Phase::Prepared, Phase::Applying)
        .unwrap();
    journal
        .record_step(op, 0, StepPhase::Ready, StepPhase::Attempting, None)
        .unwrap();

    effects.unreadable_retains.insert(step.retain);
    let alternate = step.alternate_displacement();
    effects.backups.insert(
        alternate,
        MemFile {
            bytes: EDIT.to_vec(),
            fingerprint: fingerprint(EDIT, 42),
        },
    );
    drop(journal);
    let mut journal = OperationJournal::open(dir.path()).unwrap();
    let status = recover_pending(&mut journal, &mut effects, &root(1), &root(2))
        .unwrap()
        .unwrap();
    assert_eq!(status.blocker, Some(DeployError::RecoveryConflict));
    // An unreadable canonical slot is evidence, not a known retention; the
    // positively observed alternate stays reported.
    assert!(!status.retained.contains(&step.retain));
    assert!(status.retained.contains(&alternate));
    assert_eq!(journal.unexpected_displacements(op).unwrap().len(), 2);
    assert_eq!(effects.mutations, 0);
    assert_eq!(effects.public[FILE].bytes, BASE);

    // A second recovery must not retry the effect either.
    let again = recover_pending(&mut journal, &mut effects, &root(1), &root(2))
        .unwrap()
        .unwrap();
    assert_eq!(again.blocker, Some(DeployError::RecoveryConflict));
    assert_eq!(effects.mutations, 0);
    assert_eq!(
        journal.root_state(&root(2)).unwrap(),
        RootState {
            pending: Some(op),
            active: None
        }
    );
}

#[test]
fn a_slot_confirmed_at_apply_is_not_retained_after_recovery_finds_it_unreadable() {
    let dir = fixture_v1();
    let mut journal = open_fixture(&dir);
    let mut effects = MemEffects::new();
    let plan = save_plan(&mut journal);
    let op = journal.claim(Claim::Deploy(plan)).unwrap();
    let step = deploy_step(op);
    journal.seal(op, vec![step.clone()]).unwrap();
    journal
        .set_phase(op, Phase::Prepared, Phase::Applying)
        .unwrap();
    journal
        .record_step(op, 0, StepPhase::Ready, StepPhase::Attempting, None)
        .unwrap();
    journal
        .record_step(
            op,
            0,
            StepPhase::Attempting,
            StepPhase::Applied,
            Some(applied()),
        )
        .unwrap();
    effects.public.insert(
        FILE.to_owned(),
        MemFile {
            bytes: POST.to_vec(),
            fingerprint: fingerprint(POST, 0),
        },
    );
    // The earlier `Applied` observation saw the canonical slot. A restart now
    // records the same slot unreadable; that later evidence must win.
    effects.unreadable_retains.insert(step.retain);

    drop(journal);
    let mut journal = OperationJournal::open(dir.path()).unwrap();
    let status = recover_pending(&mut journal, &mut effects, &root(1), &root(2))
        .unwrap()
        .unwrap();
    assert_eq!(status.blocker, Some(DeployError::RecoveryConflict));
    assert!(!status.retained.contains(&step.retain));
    assert_eq!(effects.mutations, 0);
    assert_eq!(effects.public[FILE].bytes, POST);
}

#[test]
fn unreadable_target_is_not_reported_as_displacement() {
    let dir = fixture_v1();
    let mut journal = open_fixture(&dir);
    let mut effects = MemEffects::new();
    let plan = save_plan(&mut journal);
    let op = journal.claim(Claim::Deploy(plan)).unwrap();
    let step = deploy_step(op);
    journal.seal(op, vec![step.clone()]).unwrap();
    journal
        .set_phase(op, Phase::Prepared, Phase::Applying)
        .unwrap();
    journal
        .record_step(op, 0, StepPhase::Ready, StepPhase::Attempting, None)
        .unwrap();

    effects.staged.insert(
        StageRef::from_record(op, 0, StepDirection::Forward),
        MemFile {
            bytes: POST.to_vec(),
            fingerprint: fingerprint(POST, 0),
        },
    );
    for slot in [BackupSlot::Original, BackupSlot::Step] {
        effects.store_backup(
            BackupRef::from_record(op, 0, StepDirection::Forward, slot),
            MemFile {
                bytes: BASE.to_vec(),
                fingerprint: fingerprint(BASE, 10),
            },
        );
    }
    effects.unreadable_target = true;
    drop(journal);
    let mut journal = OperationJournal::open(dir.path()).unwrap();
    let status = recover_pending(&mut journal, &mut effects, &root(1), &root(2))
        .unwrap()
        .unwrap();
    assert_eq!(status.blocker, Some(DeployError::RecoveryConflict));
    // The unreadable target also hides the canonical retain; the observation
    // is kept but is not advertised as a retention or treated as a conflict.
    assert!(journal.has_unresolved_displacement(op).unwrap());
    let preserved = journal.unexpected_displacements(op).unwrap();
    assert_eq!(preserved.len(), 1);
    assert_eq!(preserved[0].0, step.retain);
    assert_eq!(preserved[0].1.retained, Artifact::Unavailable);
    assert!(matches!(preserved[0].1.target, Observation::Unavailable(_)));
    assert!(!status.retained.contains(&step.retain));
    assert!(!status.retained.contains(&step.alternate_displacement()));
    assert_eq!(effects.mutations, 0);
    assert_eq!(effects.public[FILE].bytes, BASE);
    assert_eq!(
        journal.root_state(&root(2)).unwrap(),
        RootState {
            pending: Some(op),
            active: None
        }
    );

    // A readable target lets the same pending operation roll back.
    effects.unreadable_target = false;
    let resolved = recover_pending(&mut journal, &mut effects, &root(1), &root(2))
        .unwrap()
        .unwrap();
    assert_eq!(
        (resolved.phase, resolved.blocker),
        (Phase::RolledBack, None)
    );
    assert_eq!(effects.mutations, 0);
    assert_eq!(effects.public[FILE].bytes, BASE);
    assert_eq!(
        journal.root_state(&root(2)).unwrap(),
        RootState {
            pending: None,
            active: None
        }
    );
}

fn displacement(target: Observation, retained: Artifact) -> Evidence {
    Evidence {
        target,
        stage: Artifact::Absent,
        retained,
    }
}

#[test]
fn repeated_recovery_keeps_the_latest_observation_per_slot() {
    let dir = fixture_v1();
    let mut journal = open_fixture(&dir);
    let effects = MemEffects::new();
    let plan = save_plan(&mut journal);
    let op = journal.claim(Claim::Deploy(plan)).unwrap();
    let step = deploy_step(op);
    journal.seal(op, vec![step.clone()]).unwrap();

    let readable = displacement(
        Observation::Regular(fingerprint(POST, 0)),
        Artifact::Regular(fingerprint(EDIT, 42)),
    );
    let unreadable = displacement(
        Observation::Unavailable(RootError::TargetUnavailable),
        Artifact::Unavailable,
    );
    journal
        .block_displacement(op, &step, step.retain, &readable)
        .unwrap();
    journal
        .block_displacement(op, &step, step.retain, &unreadable)
        .unwrap();
    let alternate = displacement(
        Observation::Regular(fingerprint(POST, 0)),
        Artifact::Regular(fingerprint(BASE, 11)),
    );
    journal
        .block_displacement(op, &step, step.alternate_displacement(), &alternate)
        .unwrap();

    let entries = journal.unexpected_displacements(op).unwrap();
    assert_eq!(entries.len(), 2, "distinct slots keep their history");
    assert_eq!(
        entries
            .iter()
            .find(|(at, _)| *at == step.retain)
            .map(|(_, evidence)| evidence),
        Some(&unreadable),
        "the latest observation for the exact slot wins"
    );
    assert_eq!(
        entries
            .iter()
            .find(|(at, _)| *at == step.alternate_displacement())
            .map(|(_, evidence)| evidence),
        Some(&alternate)
    );
    drop(journal);

    let mut journal = OperationJournal::open(dir.path()).unwrap();
    assert_eq!(
        journal.unexpected_displacements(op).unwrap(),
        entries,
        "the latest observation survives reopen"
    );
    let status = journal.status(op).unwrap().unwrap();
    assert_eq!(status.blocker, Some(DeployError::RecoveryConflict));
    // The latest observation for the canonical slot is unreadable, so the
    // earlier readable record no longer advertises it as retained. The
    // historical alternate observation still does.
    assert!(!status.retained.contains(&step.retain));
    assert!(status.retained.contains(&step.alternate_displacement()));
    assert_eq!(effects.mutations, 0);
}

#[test]
fn simultaneous_target_and_retain_unreadability_is_preserved() {
    let dir = fixture_v1();
    let mut journal = open_fixture(&dir);
    let mut effects = MemEffects::new();
    let plan = save_plan(&mut journal);
    let op = journal.claim(Claim::Deploy(plan)).unwrap();
    let step = deploy_step(op);
    journal.seal(op, vec![step.clone()]).unwrap();
    journal
        .set_phase(op, Phase::Prepared, Phase::Applying)
        .unwrap();
    journal
        .record_step(op, 0, StepPhase::Ready, StepPhase::Attempting, None)
        .unwrap();

    effects.unreadable_target = true;
    effects.unreadable_retains.insert(step.retain);
    drop(journal);
    let mut journal = OperationJournal::open(dir.path()).unwrap();
    let status = recover_pending(&mut journal, &mut effects, &root(1), &root(2))
        .unwrap()
        .unwrap();
    // The preserved unreadable slot is unresolved evidence, not a known
    // retention, so status must not advertise it as known.
    assert_eq!(status.blocker, Some(DeployError::RecoveryConflict));
    assert!(!status.retained.contains(&step.retain));
    let entries = journal.unexpected_displacements(op).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0, step.retain);
    assert_eq!(entries[0].1.retained, Artifact::Unavailable);
    assert!(matches!(entries[0].1.target, Observation::Unavailable(_)));
    assert!(journal.has_unresolved_displacement(op).unwrap());
    journal.reopen().unwrap();
    let after = journal.status(op).unwrap().unwrap();
    assert_eq!(after.blocker, Some(DeployError::RecoveryConflict));
    assert!(!after.retained.contains(&step.retain));
    assert!(journal.has_unresolved_displacement(op).unwrap());
    assert_eq!(effects.mutations, 0);
}

#[test]
fn undo_slot_keeps_its_latest_observation() {
    let dir = fixture_v1();
    let mut journal = open_fixture(&dir);
    let mut effects = MemEffects::new();
    let plan = save_plan(&mut journal);
    let op = journal.claim(Claim::Deploy(plan)).unwrap();
    let forward = deploy_step(op);
    journal.seal(op, vec![forward.clone()]).unwrap();
    effects.remember(forward.clone());
    journal
        .set_phase(op, Phase::Prepared, Phase::Applying)
        .unwrap();
    journal
        .record_step(op, 0, StepPhase::Ready, StepPhase::Attempting, None)
        .unwrap();
    journal
        .record_step(
            op,
            0,
            StepPhase::Attempting,
            StepPhase::Applied,
            Some(applied()),
        )
        .unwrap();
    journal
        .set_phase(op, Phase::Applying, Phase::RollingBack)
        .unwrap();
    let undo = PreparedStep {
        op,
        ordinal: 0,
        direction: StepDirection::Undo,
        spec: StepSpec {
            target: bound(),
            original: forward.spec.original.clone(),
            pre: PreState::Regular(fingerprint(POST, 0)),
            output: StepOutput::Original(forward.step_pre.backup.unwrap()),
        },
        step_pre: PreImage {
            observation: PreState::Regular(fingerprint(POST, 0)),
            backup: Some(BackupRef::from_record(
                op,
                0,
                StepDirection::Undo,
                BackupSlot::Step,
            )),
        },
        staged: Some((
            StageRef::from_record(op, 0, StepDirection::Undo),
            fingerprint(BASE, 0),
        )),
        retain: BackupRef::from_record(op, 0, StepDirection::Undo, BackupSlot::Displaced),
    };
    journal.record_undo(op, 0, undo.clone()).unwrap();
    effects.remember(undo.clone());

    let readable = displacement(
        Observation::Regular(fingerprint(BASE, 0)),
        Artifact::Regular(fingerprint(EDIT, 42)),
    );
    let unreadable = displacement(
        Observation::Unavailable(RootError::TargetUnavailable),
        Artifact::Unavailable,
    );
    journal
        .block_displacement(op, &undo, undo.retain, &readable)
        .unwrap();
    journal
        .block_displacement(op, &undo, undo.retain, &unreadable)
        .unwrap();

    let entries = journal.unexpected_displacements(op).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0, undo.retain);
    assert_eq!(entries[0].0.direction(), StepDirection::Undo);
    assert_eq!(entries[0].1, unreadable);
    assert_eq!(effects.mutations, 0);
}

#[test]
fn repeated_recovery_replaces_an_earlier_unreadable_observation() {
    let dir = fixture_v1();
    let mut journal = open_fixture(&dir);
    let mut effects = MemEffects::new();
    let plan = save_plan(&mut journal);
    let op = journal.claim(Claim::Deploy(plan)).unwrap();
    let step = deploy_step(op);
    journal.seal(op, vec![step.clone()]).unwrap();
    journal
        .set_phase(op, Phase::Prepared, Phase::Applying)
        .unwrap();
    journal
        .record_step(op, 0, StepPhase::Ready, StepPhase::Attempting, None)
        .unwrap();
    drop(journal);

    let mut journal = OperationJournal::open(dir.path()).unwrap();
    effects.unreadable_target = true;
    let first = recover_pending(&mut journal, &mut effects, &root(1), &root(2))
        .unwrap()
        .unwrap();
    assert_eq!(first.blocker, Some(DeployError::RecoveryConflict));
    assert!(!first.retained.contains(&step.retain));
    let entries = journal.unexpected_displacements(op).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].1.retained, Artifact::Unavailable);

    // The next pass can read both slots and now observes a real displacement
    // at the same canonical ref. That later observation must replace the
    // earlier unreadable record rather than being dropped by ON CONFLICT.
    effects.unreadable_target = false;
    let displaced = MemFile {
        bytes: EDIT.to_vec(),
        fingerprint: fingerprint(EDIT, 42),
    };
    effects.backups.insert(step.retain, displaced.clone());
    let second = recover_pending(&mut journal, &mut effects, &root(1), &root(2))
        .unwrap()
        .unwrap();
    assert_eq!(second.blocker, Some(DeployError::RecoveryConflict));
    assert!(second.retained.contains(&step.retain));
    let entries = journal.unexpected_displacements(op).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].1.retained,
        Artifact::Regular(displaced.fingerprint.clone())
    );
    assert_eq!(effects.mutations, 0);
}

fn slot_history(dir: &TempDir, op: OperationId) -> i64 {
    Connection::open(dir.path().join("catalog.sqlite3"))
        .unwrap()
        .query_row(
            "SELECT count(*) FROM displacement_observations WHERE operation_id = ?1",
            [op.get() as i64],
            |row| row.get(0),
        )
        .unwrap()
}

fn attempting_fixture() -> (TempDir, OperationJournal, PreparedStep) {
    let dir = fixture_v1();
    let mut journal = open_fixture(&dir);
    let plan = save_plan(&mut journal);
    let op = journal.claim(Claim::Deploy(plan)).unwrap();
    let step = deploy_step(op);
    journal.seal(op, vec![step.clone()]).unwrap();
    journal
        .set_phase(op, Phase::Prepared, Phase::Applying)
        .unwrap();
    journal
        .record_step(op, 0, StepPhase::Ready, StepPhase::Attempting, None)
        .unwrap();
    (dir, journal, step)
}

#[test]
fn readability_degrading_keeps_history_and_advertises_only_a_latest_positive_read() {
    let (dir, mut journal, step) = attempting_fixture();
    let op = step.op;
    let mut effects = MemEffects::new();
    let displaced = MemFile {
        bytes: EDIT.to_vec(),
        fingerprint: fingerprint(EDIT, 42),
    };
    effects.backups.insert(step.retain, displaced);

    drop(journal);
    let mut journal = OperationJournal::open(dir.path()).unwrap();
    let first = recover_pending(&mut journal, &mut effects, &root(1), &root(2))
        .unwrap()
        .unwrap();
    assert_eq!(first.blocker, Some(DeployError::RecoveryConflict));
    assert!(first.retained.contains(&step.retain));
    assert!(journal.has_unresolved_displacement(op).unwrap());
    assert_eq!(slot_history(&dir, op), 1);

    // The same slot now reads unreadable. The readable row stays durable as
    // history, but only the latest observation may decide status.
    effects.backups.remove(&step.retain);
    effects.unreadable_retains.insert(step.retain);
    let second = recover_pending(&mut journal, &mut effects, &root(1), &root(2))
        .unwrap()
        .unwrap();
    assert_eq!(second.blocker, Some(DeployError::RecoveryConflict));
    assert!(!second.retained.contains(&step.retain));
    assert!(journal.has_unresolved_displacement(op).unwrap());
    assert_eq!(
        slot_history(&dir, op),
        2,
        "the earlier readable observation remains durable history"
    );

    journal.reopen().unwrap();
    let after = journal.status(op).unwrap().unwrap();
    assert_eq!(after.blocker, Some(DeployError::RecoveryConflict));
    assert!(!after.retained.contains(&step.retain));
    assert!(journal.has_unresolved_displacement(op).unwrap());
    assert_eq!(effects.mutations, 0);
}
