//! Review-fix coverage for the real operation journal: restore original-ref
//! binding, undo slots, zero-step finalization, shared schema and decode guards.

use std::path::Path;

use marduk_core::deployment::*;
use marduk_core::ids::{InstalledRevisionId, ProfileId, ProfileRevision};
use marduk_core::recovery::*;
use marduk_core::root_identity::{FileMetadata, Fingerprint, NativeIdentity, RootIdentity};
use marduk_core::targets::{RelativePath, RootRole, TargetFile};
use marduk_storage::operation_journal::OperationJournal;
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const DB: &str = "catalog.sqlite3";
const FILE: &str = "textures/a-only.dds";

fn native(id: u8) -> NativeIdentity {
    NativeIdentity::Windows {
        volume_serial: 71,
        file_id: [id; 16],
        creation_100ns: 201,
    }
}

fn root(id: u8) -> RootIdentity {
    RootIdentity::from_adapter_evidence(native(id))
}

fn target() -> TargetFile {
    TargetFile {
        root: RootRole::Data,
        relative: RelativePath::parse(FILE).unwrap(),
    }
}

fn bound() -> BoundTarget {
    BoundTarget {
        target: target(),
        parent: root(3),
        leaf: "a-only.dds".into(),
    }
}

fn source_hash() -> [u8; 32] {
    Sha256::digest(b"only-a\n").into()
}

fn fingerprint(ino: u8, len: u64) -> Fingerprint {
    Fingerprint {
        identity: native(ino),
        len,
        sha256: source_hash(),
        metadata: FileMetadata::Windows {
            attributes: 0x20,
            last_write_100ns: 301 + i64::from(ino),
            links: 1,
            security_sha256: [71; 32],
        },
    }
}

fn original_fp() -> Fingerprint {
    fingerprint(4, 7)
}

fn post_fp() -> Fingerprint {
    fingerprint(9, 7)
}

fn source() -> SourceRef {
    SourceRef {
        installed: InstalledRevisionId::new(1),
        path: RelativePath::parse(FILE).unwrap(),
        sha256: source_hash(),
        len: 7,
    }
}

fn fixture_v1() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let db = Connection::open(dir.path().join(DB)).unwrap();
    db.execute_batch(include_str!("../migrations/0001_library.sql"))
        .unwrap();
    db.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA user_version = 1;
         INSERT INTO profiles (id, revision) VALUES (1, 7);
         INSERT INTO imports
             (id, profile_id, expected_revision, target, display_name, state, owner, archive_sha, result_revision)
             VALUES (1, 1, 6, 'append', 'fixture', 'committed', x'01', zeroblob(32), 7);
         INSERT INTO installed_revisions (id) VALUES (1);
         INSERT INTO members (id, profile_id, ordinal, installed_id, enabled, display_name)
             VALUES (1, 1, 0, 1, 1, 'fixture');",
    )
    .unwrap();
    db.execute(
        "INSERT INTO revision_files (revision_id, ordinal, role, path, len, sha)
         VALUES (1, 0, 'data', ?1, 7, ?2)",
        params![FILE, source_hash().as_slice()],
    )
    .unwrap();
    dir
}

fn open_fixture(dir: &TempDir) -> OperationJournal {
    let mut journal = OperationJournal::open(dir.path()).unwrap();
    journal
        .select_verified_installation(Path::new("synthetic-installation"), &root(1), &root(2))
        .unwrap();
    journal
}

fn intent() -> FileIntent {
    FileIntent {
        target: bound(),
        source: source(),
        original: PreState::Regular(original_fp()),
    }
}

fn plan(journal: &mut OperationJournal) -> DeploymentPlan {
    journal
        .save_plan(
            ProfileId::new(1),
            ProfileRevision::new(7),
            root(1),
            root(2),
            vec![intent()],
        )
        .unwrap()
}

fn empty_plan(journal: &mut OperationJournal) -> DeploymentPlan {
    journal
        .save_plan(
            ProfileId::new(1),
            ProfileRevision::new(7),
            root(1),
            root(2),
            Vec::new(),
        )
        .unwrap()
}

fn original_ref(op: OperationId) -> BackupRef {
    BackupRef::from_record(op, 0, StepDirection::Forward, BackupSlot::Original)
}

fn deploy_step(op: OperationId) -> PreparedStep {
    PreparedStep {
        op,
        ordinal: 0,
        direction: StepDirection::Forward,
        spec: StepSpec {
            target: bound(),
            original: PreImage {
                observation: PreState::Regular(original_fp()),
                backup: Some(original_ref(op)),
            },
            pre: PreState::Regular(original_fp()),
            output: StepOutput::Source(source()),
        },
        step_pre: PreImage {
            observation: PreState::Regular(original_fp()),
            backup: Some(BackupRef::from_record(
                op,
                0,
                StepDirection::Forward,
                BackupSlot::Step,
            )),
        },
        staged: Some((
            StageRef::from_record(op, 0, StepDirection::Forward),
            post_fp(),
        )),
        retain: BackupRef::from_record(op, 0, StepDirection::Forward, BackupSlot::Displaced),
    }
}

fn restore_step(op: OperationId, original: BackupRef) -> PreparedStep {
    PreparedStep {
        op,
        ordinal: 0,
        direction: StepDirection::Forward,
        spec: StepSpec {
            target: bound(),
            original: PreImage {
                observation: PreState::Regular(original_fp()),
                backup: Some(original),
            },
            pre: PreState::Regular(post_fp()),
            output: StepOutput::Original(original),
        },
        step_pre: PreImage {
            observation: PreState::Regular(post_fp()),
            backup: Some(BackupRef::from_record(
                op,
                0,
                StepDirection::Forward,
                BackupSlot::Step,
            )),
        },
        staged: Some((
            StageRef::from_record(op, 0, StepDirection::Forward),
            original_fp(),
        )),
        retain: BackupRef::from_record(op, 0, StepDirection::Forward, BackupSlot::Displaced),
    }
}

fn applied() -> Evidence {
    Evidence {
        target: Observation::Regular(post_fp()),
        stage: Artifact::Absent,
        retained: Artifact::Regular(original_fp()),
    }
}

fn restore_applied() -> Evidence {
    Evidence {
        target: Observation::Regular(original_fp()),
        stage: Artifact::Absent,
        retained: Artifact::Regular(post_fp()),
    }
}

fn applying_deploy(journal: &mut OperationJournal) -> (OperationId, PreparedStep) {
    let plan = plan(journal);
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
    (op, step)
}

fn completed_deploy(journal: &mut OperationJournal) -> (OperationId, GenerationId, BackupRef) {
    let (op, _) = applying_deploy(journal);
    let generation = journal
        .finish(op, Finalize::DeployCommitted)
        .unwrap()
        .generation
        .unwrap();
    (op, generation, original_ref(op))
}

#[test]
fn restore_seals_and_reads_the_generation_deploy_original() {
    let dir = fixture_v1();
    let mut journal = open_fixture(&dir);
    let (_deploy, generation, original) = completed_deploy(&mut journal);

    let restore = journal
        .claim(Claim::Restore {
            installation: root(1),
            data: root(2),
            generation,
        })
        .unwrap();
    let step = restore_step(restore, original);
    journal.seal(restore, vec![step.clone()]).unwrap();

    let read = journal.steps(restore).unwrap();
    let [row] = read.as_slice() else {
        panic!("one restore step required");
    };
    assert_eq!(row.prepared.spec.original.backup, Some(original));
    assert_eq!(row.prepared.spec.output, StepOutput::Original(original));

    journal
        .set_phase(restore, Phase::Prepared, Phase::Restoring)
        .unwrap();
    journal
        .record_step(restore, 0, StepPhase::Ready, StepPhase::Attempting, None)
        .unwrap();
    journal
        .record_step(
            restore,
            0,
            StepPhase::Attempting,
            StepPhase::Applied,
            Some(restore_applied()),
        )
        .unwrap();
    let status = journal.finish(restore, Finalize::RestoreCompleted).unwrap();
    assert_eq!((status.phase, status.generation), (Phase::Completed, None));
    let state = journal.root_state(&root(2)).unwrap();
    assert_eq!((state.pending, state.active), (None, None));
}

#[test]
fn restore_seal_rejects_a_ref_owned_by_the_restore_operation() {
    let dir = fixture_v1();
    let mut journal = open_fixture(&dir);
    let (_deploy, generation, original) = completed_deploy(&mut journal);
    let restore = journal
        .claim(Claim::Restore {
            installation: root(1),
            data: root(2),
            generation,
        })
        .unwrap();
    let wrong = BackupRef::from_record(restore, 0, StepDirection::Forward, BackupSlot::Original);
    assert!(
        journal
            .seal(restore, vec![restore_step(restore, wrong)])
            .is_err()
    );
}

#[test]
fn undo_accepts_only_undo_slots_and_reads_back() {
    let dir = fixture_v1();
    let mut journal = open_fixture(&dir);
    let (op, forward) = applying_deploy(&mut journal);
    let step_ref = forward.step_pre.backup.unwrap();

    let undo = PreparedStep {
        op,
        ordinal: 0,
        direction: StepDirection::Undo,
        spec: StepSpec {
            target: bound(),
            original: forward.spec.original.clone(),
            pre: PreState::Regular(post_fp()),
            output: StepOutput::Original(step_ref),
        },
        step_pre: PreImage {
            observation: PreState::Regular(post_fp()),
            backup: Some(BackupRef::from_record(
                op,
                0,
                StepDirection::Undo,
                BackupSlot::Step,
            )),
        },
        staged: Some((
            StageRef::from_record(op, 0, StepDirection::Undo),
            original_fp(),
        )),
        retain: BackupRef::from_record(op, 0, StepDirection::Undo, BackupSlot::Displaced),
    };

    // The forward displaced slot is not a valid undo retain.
    let mut wrong = undo.clone();
    wrong.retain = BackupRef::from_record(op, 0, StepDirection::Forward, BackupSlot::Displaced);
    assert!(journal.record_undo(op, 0, wrong).is_err());

    journal
        .record_step(op, 0, StepPhase::Applied, StepPhase::UndoAttempting, None)
        .unwrap();
    journal.record_undo(op, 0, undo.clone()).unwrap();
    assert!(journal.record_undo(op, 0, undo.clone()).is_err());

    let rows = journal.steps(op).unwrap();
    let [row] = rows.as_slice() else {
        panic!("one undo step required");
    };
    assert_eq!(row.undo.as_ref(), Some(&undo));

    journal
        .record_step(
            op,
            0,
            StepPhase::UndoAttempting,
            StepPhase::Undone,
            Some(Evidence {
                target: Observation::Regular(original_fp()),
                stage: Artifact::Absent,
                retained: Artifact::Regular(post_fp()),
            }),
        )
        .unwrap();
    journal
        .set_phase(op, Phase::Applying, Phase::RollingBack)
        .unwrap();
    let status = journal.finish(op, Finalize::RolledBack).unwrap();
    assert_eq!(status.phase, Phase::RolledBack);
    let state = journal.root_state(&root(2)).unwrap();
    assert_eq!((state.pending, state.active), (None, None));
}

#[test]
fn zero_step_deploy_and_restore_release_the_lease() {
    let dir = fixture_v1();
    let mut journal = open_fixture(&dir);

    let empty = empty_plan(&mut journal);
    let deploy = journal.claim(Claim::Deploy(empty)).unwrap();
    journal.seal(deploy, Vec::new()).unwrap();
    journal
        .set_phase(deploy, Phase::Prepared, Phase::Applying)
        .unwrap();
    let status = journal.finish(deploy, Finalize::DeployCommitted).unwrap();
    let generation = status.generation.unwrap();
    let record = journal.generation(generation).unwrap();
    assert!(record.steps.is_empty());
    let state = journal.root_state(&root(2)).unwrap();
    assert_eq!((state.pending, state.active), (None, Some(generation)));

    // A later deploy is refused for the active generation, not a wedged Busy lease.
    let later = plan(&mut journal);
    assert!(matches!(
        journal.claim(Claim::Deploy(later)),
        Err(DeployError::ActiveDeployment)
    ));

    let restore = journal
        .claim(Claim::Restore {
            installation: root(1),
            data: root(2),
            generation,
        })
        .unwrap();
    journal.seal(restore, Vec::new()).unwrap();
    journal
        .set_phase(restore, Phase::Prepared, Phase::Restoring)
        .unwrap();
    journal.finish(restore, Finalize::RestoreCompleted).unwrap();
    let state = journal.root_state(&root(2)).unwrap();
    assert_eq!((state.pending, state.active), (None, None));

    let later = plan(&mut journal);
    assert!(journal.claim(Claim::Deploy(later)).is_ok());
}

#[test]
fn seal_guards_pre_state_and_restore_bindings() {
    let dir = fixture_v1();
    let mut journal = open_fixture(&dir);

    let plan = plan(&mut journal);
    let op = journal.claim(Claim::Deploy(plan)).unwrap();
    let mut step = deploy_step(op);
    step.step_pre.observation = PreState::Absent;
    step.step_pre.backup = None;
    assert!(journal.seal(op, vec![step]).is_err());
    journal.finish(op, Finalize::Cancelled).unwrap();

    let (_deploy, generation, original) = completed_deploy(&mut journal);
    let restore = journal
        .claim(Claim::Restore {
            installation: root(1),
            data: root(2),
            generation,
        })
        .unwrap();
    let mut wrong_ordinal = restore_step(restore, original);
    wrong_ordinal.ordinal = 1;
    wrong_ordinal.spec.original.backup = Some(original);
    assert!(journal.seal(restore, vec![wrong_ordinal]).is_err());
}

#[test]
fn decode_rejects_applied_without_evidence_and_block_on_terminal() {
    let dir = fixture_v1();
    let mut journal = open_fixture(&dir);
    let (op, generation, _) = completed_deploy(&mut journal);

    let witness = Connection::open(dir.path().join(DB)).unwrap();
    witness
        .execute(
            "UPDATE operation_steps SET observed = NULL WHERE operation_id = ?1 AND ordinal = 0",
            [i64::try_from(op.get()).unwrap()],
        )
        .unwrap();
    assert!(journal.steps(op).is_err());

    assert!(matches!(
        journal.block(op, Some(DeployError::Io)),
        Err(DeployError::UnknownOperation)
    ));
    let state = journal.root_state(&root(2)).unwrap();
    assert_eq!(state.active, Some(generation));
}
