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
    assert_eq!(status.blocker, Some(DeployError::TargetUnavailable));
    assert!(journal.unexpected_displacements(op).unwrap().is_empty());
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
