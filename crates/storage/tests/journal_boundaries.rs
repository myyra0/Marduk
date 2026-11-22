include!("operation_journal.rs");

#[test]
fn editing_only_the_revision_does_not_refresh_an_approved_plan() {
    let dir = fixture_v1();
    let mut journal = open_fixture(&dir);
    let mut reviewed = plan(&mut journal);
    let db = Connection::open(dir.path().join(DB)).unwrap();
    db.execute("UPDATE profiles SET revision = 8", []).unwrap();
    db.execute("UPDATE deployment_plans SET revision = 8", [])
        .unwrap();
    assert!(journal.plan(reviewed.id).is_err());
    reviewed.revision = ProfileRevision::new(8);
    assert!(journal.claim(Claim::Deploy(reviewed)).is_err());
    root_state(&mut journal, None, None);
}

#[test]
fn seal_rejects_header_changed_after_claim() {
    let (dir, mut journal, op) = claimed_fixture();
    Connection::open(dir.path().join(DB))
        .unwrap()
        .execute("UPDATE deployment_plans SET revision = 8", [])
        .unwrap();
    assert!(journal.seal(op, vec![prepared(op)]).is_err());
    root_state(&mut journal, Some(op), None);
}

#[test]
fn swapping_the_plan_root_pair_invalidates_approval() {
    let dir = fixture_v1();
    let mut journal = open_fixture(&dir);
    let mut reviewed = plan(&mut journal);
    reviewed.installation = root(5);
    reviewed.data = root(6);
    journal
        .select_verified_installation(Path::new("other"), &reviewed.installation, &reviewed.data)
        .unwrap();
    Connection::open(dir.path().join(DB))
        .unwrap()
        .execute(
            "UPDATE deployment_plans SET (installation_id, installation, data) =
         (SELECT id, installation, data FROM installations WHERE selected = 1)",
            [],
        )
        .unwrap();
    assert_eq!(journal.plan(reviewed.id), Err(DeployError::StalePlan));
    assert_eq!(
        journal.claim(Claim::Deploy(reviewed.clone())),
        Err(DeployError::StalePlan)
    );
    assert_eq!(
        journal.root_state(&reviewed.data).unwrap(),
        RootState {
            pending: None,
            active: None
        }
    );
}

fn claimed_fixture() -> (TempDir, OperationJournal, OperationId) {
    let dir = fixture_v1();
    let mut journal = open_fixture(&dir);
    let reviewed = plan(&mut journal);
    let op = journal.claim(Claim::Deploy(reviewed)).unwrap();
    (dir, journal, op)
}

#[test]
fn seal_rejects_unapproved_parent_and_source() {
    let (_dir, mut journal, op) = claimed_fixture();
    let mut step = prepared(op);
    step.spec.target.parent = root(6);
    assert!(journal.seal(op, vec![step]).is_err());
    let mut step = prepared(op);
    let StepOutput::Source(source) = &mut step.spec.output else {
        unreachable!()
    };
    source.path = RelativePath::parse("other.dds").unwrap();
    assert!(journal.seal(op, vec![step]).is_err());
}

#[test]
fn cannot_enter_public_phase_without_seal_or_skip_rollback_resolution() {
    let (_dir, mut journal, op) = claimed_fixture();
    assert!(
        journal
            .set_phase(op, Phase::Preparing, Phase::Applying)
            .is_err()
    );
    journal.seal(op, vec![prepared(op)]).unwrap();
    journal
        .set_phase(op, Phase::Prepared, Phase::Applying)
        .unwrap();
    assert!(journal.finish(op, Finalize::RolledBack).is_err());
    journal
        .set_phase(op, Phase::Applying, Phase::RollingBack)
        .unwrap();
    assert!(journal.finish(op, Finalize::RolledBack).is_err());
    root_state(&mut journal, Some(op), None);
}

#[test]
fn missing_sealed_rows_cannot_commit_a_generation() {
    let (dir, mut journal, op) = claimed_fixture();
    journal.seal(op, vec![prepared(op)]).unwrap();
    journal
        .set_phase(op, Phase::Prepared, Phase::Applying)
        .unwrap();
    Connection::open(dir.path().join(DB))
        .unwrap()
        .execute("DELETE FROM operation_steps", [])
        .unwrap();
    assert!(journal.finish(op, Finalize::DeployCommitted).is_err());
    root_state(&mut journal, Some(op), None);
}

#[test]
fn setup_cannot_deselect_pending_pair() {
    let (_dir, mut journal, op) = claimed_fixture();
    assert_eq!(
        journal.select_verified_installation(Path::new("other"), &root(5), &root(6)),
        Err(DeployError::Busy)
    );
    root_state(&mut journal, Some(op), None);
}

#[test]
fn catalog_writer_guard_can_enclose_journal_writes() {
    use marduk_platform::{GateError, WriterGate};
    use marduk_storage::library::Catalog;

    let dir = fixture_v1();
    let mut catalog = Catalog::open(dir.path()).unwrap();
    let mut journal = open_fixture(&dir);
    let library = WriterGate::open(catalog.root().join("library.lock")).unwrap();
    let _guard = catalog.lock().unwrap();
    let reviewed = plan(&mut journal);
    let op = journal.claim(Claim::Deploy(reviewed)).unwrap();
    journal.seal(op, vec![prepared(op)]).unwrap();
    assert!(library.try_lock().is_err());
    let journal_gate = WriterGate::open(catalog.root().join("journal.lock")).unwrap();
    let _journal_guard = journal_gate.try_lock().unwrap();
    assert!(matches!(journal_gate.try_lock(), Err(GateError::Busy)));
    assert_eq!(
        journal.set_phase(op, Phase::Prepared, Phase::Applying),
        Err(DeployError::Busy)
    );
    assert_eq!(
        catalog.snapshot(ProfileId::new(1)).unwrap().revision,
        ProfileRevision::new(7)
    );
}

#[test]
fn deleted_approved_intents_do_not_prove_an_empty_plan() {
    let (dir, mut journal, op) = claimed_fixture();
    Connection::open(dir.path().join(DB))
        .unwrap()
        .execute("DELETE FROM plan_intents", [])
        .unwrap();
    assert!(journal.seal(op, Vec::new()).is_err());
}

#[test]
fn status_does_not_report_a_foreign_backup_ref() {
    let (dir, mut journal, op) = claimed_fixture();
    journal.seal(op, vec![prepared(op)]).unwrap();
    Connection::open(dir.path().join(DB))
        .unwrap()
        .execute("UPDATE operation_steps SET displaced_ref = x'00'", [])
        .unwrap();
    assert!(journal.status(op).is_err());
}

#[test]
fn recovery_pair_and_locator_must_match_the_claim() {
    let (dir, mut journal, op) = claimed_fixture();
    let location = journal.verify_pending_pair(op, &root(1), &root(2)).unwrap();
    assert!(location.is_some());
    assert_eq!(
        journal.verify_pending_pair(op, &root(5), &root(2)),
        Err(DeployError::TargetUnavailable)
    );
    journal
        .select_verified_installation(Path::new("other-locator"), &root(1), &root(2))
        .unwrap();
    assert_eq!(
        journal.verify_pending_pair(op, &root(1), &root(2)).unwrap(),
        location
    );
    Connection::open(dir.path().join(DB))
        .unwrap()
        .execute("UPDATE installations SET location = x'0078'", [])
        .unwrap();
    assert_eq!(
        journal.verify_pending_pair(op, &root(1), &root(2)),
        Err(DeployError::TargetUnavailable)
    );
}

#[test]
fn unexpectedly_displaced_ref_survives_reopen_and_prevents_finalize() {
    let (dir, mut journal, op) = claimed_fixture();
    let step = prepared(op);
    journal.seal(op, vec![step.clone()]).unwrap();
    journal
        .set_phase(op, Phase::Prepared, Phase::Applying)
        .unwrap();
    journal
        .record_step(op, 0, StepPhase::Ready, StepPhase::Attempting, None)
        .unwrap();
    // A ref that was never allocated as a displacement destination is still
    // evidence of an unexpected write, but it is not a reported location.
    let at = BackupRef::from_record(op, 0, StepDirection::Forward, BackupSlot::Step);
    journal
        .block_displacement(op, &step, at, &applied())
        .unwrap();
    drop(journal);

    let mut journal = OperationJournal::open(dir.path()).unwrap();
    let status = journal.status(op).unwrap().unwrap();
    assert_eq!(status.blocker, Some(DeployError::RecoveryConflict));
    assert!(status.retained.is_empty());
    assert!(journal.unexpected_displacements(op).unwrap().is_empty());
    assert!(journal.has_unresolved_displacement(op).unwrap());
    assert!(
        journal
            .record_step(
                op,
                0,
                StepPhase::Attempting,
                StepPhase::Applied,
                Some(applied())
            )
            .is_err()
    );
    assert!(journal.finish(op, Finalize::DeployCommitted).is_err());
    root_state(&mut journal, Some(op), None);
}

#[test]
fn terminal_transaction_checks_evidence_values_not_just_applied_flag() {
    let (dir, mut journal, op) = claimed_fixture();
    journal.seal(op, vec![prepared(op)]).unwrap();
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
    let db = Connection::open(dir.path().join(DB)).unwrap();
    let mut evidence: Vec<u8> = db
        .query_row("SELECT observed FROM operation_steps", [], |row| row.get(0))
        .unwrap();
    evidence[6] ^= 1;
    db.execute("UPDATE operation_steps SET observed = ?1", [evidence])
        .unwrap();
    assert!(journal.finish(op, Finalize::DeployCommitted).is_err());
    root_state(&mut journal, Some(op), None);
}

#[test]
fn restore_seal_requires_the_committed_managed_identity() {
    let (_dir, mut journal, op) = claimed_fixture();
    journal.seal(op, vec![prepared(op)]).unwrap();
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
    let generation = journal
        .finish(op, Finalize::DeployCommitted)
        .unwrap()
        .generation
        .unwrap();
    assert_eq!(
        journal.select_verified_installation(Path::new("other"), &root(5), &root(6)),
        Err(DeployError::ActiveDeployment)
    );

    let restore = journal
        .claim(Claim::Restore {
            installation: root(1),
            data: root(2),
            generation,
        })
        .unwrap();
    let mut step = prepared(restore);
    step.staged = None;
    step.spec.output = StepOutput::Absent;
    step.step_pre.backup = Some(BackupRef::from_record(
        restore,
        0,
        StepDirection::Forward,
        BackupSlot::Step,
    ));
    let mut other = fingerprint();
    other.identity = native(5);
    step.spec.pre = PreState::Regular(other);
    step.step_pre.observation = step.spec.pre.clone();
    assert!(journal.seal(restore, vec![step.clone()]).is_err());
    step.spec.pre = PreState::Regular(fingerprint());
    step.step_pre.observation = step.spec.pre.clone();
    journal.seal(restore, vec![step]).unwrap();
}

#[cfg(unix)]
#[test]
fn journal_open_refuses_a_redirected_root_ancestor_without_writing() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), tmp.path().join("link")).unwrap();

    assert!(matches!(
        OperationJournal::open(&tmp.path().join("link").join("app-data")),
        Err(DeployError::Io)
    ));
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn journal_open_refuses_a_redirected_catalog_file_without_writing() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tmp.path().join("outside.sqlite3");
    std::fs::write(&outside, b"not a catalog").unwrap();
    std::os::unix::fs::symlink(&outside, tmp.path().join(DB)).unwrap();

    assert!(matches!(
        OperationJournal::open(tmp.path()),
        Err(DeployError::Io)
    ));
    assert_eq!(std::fs::read(&outside).unwrap(), b"not a catalog");
}

#[cfg(unix)]
#[test]
fn journal_reopen_refuses_a_catalog_replaced_by_a_link() {
    let dir = fixture_v1();
    let mut journal = open_fixture(&dir);
    let outside = dir.path().join("outside.sqlite3");
    std::fs::write(&outside, b"outside").unwrap();
    std::fs::rename(dir.path().join(DB), dir.path().join("moved.sqlite3")).unwrap();
    std::os::unix::fs::symlink(&outside, dir.path().join(DB)).unwrap();

    assert!(matches!(journal.reopen(), Err(DeployError::Io)));
    assert_eq!(std::fs::read(&outside).unwrap(), b"outside");
}

#[cfg(unix)]
#[test]
fn journal_reopen_refuses_a_substituted_app_root() {
    let dir = fixture_v1();
    let mut journal = open_fixture(&dir);
    let root = dir.path().to_path_buf();
    let moved = root.with_extension("moved");
    std::fs::rename(&root, &moved).unwrap();
    std::fs::create_dir(&root).unwrap();

    assert!(matches!(journal.reopen(), Err(DeployError::Io)));
    assert_eq!(
        std::fs::read_dir(&root).unwrap().count(),
        0,
        "a refused re-proof must not write through the replacement path"
    );
    // Restore the fixture so the temporary directory cleans itself up.
    std::fs::remove_dir(&root).unwrap();
    std::fs::rename(&moved, &root).unwrap();
}

#[cfg(windows)]
fn redirected_catalog(dir: &Path) -> Option<std::path::PathBuf> {
    let outside = dir.join("outside.sqlite3");
    std::fs::write(&outside, b"outside").unwrap();
    let catalog = dir.join(DB);
    if std::fs::rename(&catalog, dir.join("moved.sqlite3")).is_err() {
        // A fresh root has no catalog yet.
    }
    match std::os::windows::fs::symlink_file(&outside, &catalog) {
        Ok(()) => Some(outside),
        Err(_) => {
            // Creating a file symlink needs Developer Mode or elevation.
            let _ = std::fs::rename(dir.join("moved.sqlite3"), &catalog);
            None
        }
    }
}

#[cfg(windows)]
#[test]
fn bootstrap_refuses_a_redirected_catalog_leaf() {
    let dir = tempfile::tempdir().unwrap();
    let Some(outside) = redirected_catalog(dir.path()) else {
        return;
    };

    assert!(OperationJournal::open(dir.path()).is_err());
    assert_eq!(std::fs::read(&outside).unwrap(), b"outside");
}

#[cfg(windows)]
#[test]
fn reopen_refuses_a_redirected_catalog_leaf() {
    let dir = fixture_v1();
    let mut journal = open_fixture(&dir);
    let Some(outside) = redirected_catalog(dir.path()) else {
        return;
    };

    assert!(journal.reopen().is_err());
    assert_eq!(std::fs::read(&outside).unwrap(), b"outside");
}

/// A rename or delete needs delete access to the catalog leaf; the held
/// catalog handle shares read and write only, so the OS refuses both for as
/// long as the catalog is open. This is the bootstrap path, where the catalog
/// did not exist before the open.
#[cfg(windows)]
#[test]
fn bootstrap_catalog_handle_blocks_leaf_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let mut journal = OperationJournal::open(dir.path()).unwrap();
    let catalog = dir.path().join(DB);
    assert!(catalog.is_file());
    assert_replacement_refused(&catalog, dir.path());
    // The catalog is still usable through the held handle.
    journal.root_state(&root(2)).unwrap();
    drop(journal);
    std::fs::rename(&catalog, dir.path().join("after-close.sqlite3")).unwrap();
}

/// The same guarantee survives `Journal::reopen`: the leaf handle is kept and
/// re-proved, never re-created with delete sharing.
#[cfg(windows)]
#[test]
fn reopen_catalog_handle_blocks_leaf_replacement() {
    let dir = fixture_v1();
    let mut journal = open_fixture(&dir);
    journal.reopen().unwrap();
    assert_replacement_refused(&dir.path().join(DB), dir.path());
    journal.root_state(&root(2)).unwrap();
}

#[cfg(windows)]
fn assert_replacement_refused(catalog: &Path, dir: &Path) {
    let error = std::fs::rename(catalog, dir.join("replaced.sqlite3")).unwrap_err();
    assert_eq!(
        error.raw_os_error(),
        Some(32),
        "rename must be refused by the non-deleting catalog handle: {error}"
    );
    let error = std::fs::remove_file(catalog).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(32));
}
