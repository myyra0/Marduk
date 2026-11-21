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
        journal.root_state(&root(2)).unwrap(),
        RootState {
            pending: Some(op),
            active: None
        }
    );
}
