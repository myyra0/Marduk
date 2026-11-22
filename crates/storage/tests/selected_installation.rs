//! Regression witnesses split out of the frozen SQL replay test.
//!
//! The frozen `operation_journal.rs` witness stays byte-identical; these
//! additive observations of `selected_installation` live here instead.

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

fn source_hash() -> [u8; 32] {
    Sha256::digest(b"only-a\n").into()
}

fn target() -> BoundTarget {
    BoundTarget {
        target: TargetFile {
            root: RootRole::Data,
            relative: RelativePath::parse(FILE).unwrap(),
        },
        parent: root(3),
        leaf: "a-only.dds".into(),
    }
}

fn source() -> SourceRef {
    SourceRef {
        installed: InstalledRevisionId::new(1),
        path: RelativePath::parse(FILE).unwrap(),
        sha256: source_hash(),
        len: 7,
    }
}

fn fingerprint() -> Fingerprint {
    Fingerprint {
        identity: native(4),
        len: 7,
        sha256: source_hash(),
        metadata: FileMetadata::Windows {
            attributes: 0x20,
            last_write_100ns: 301,
            links: 1,
            security_sha256: [71; 32],
        },
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
    ).unwrap();
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

fn plan(journal: &mut OperationJournal) -> DeploymentPlan {
    journal
        .save_plan(
            ProfileId::new(1),
            ProfileRevision::new(7),
            root(1),
            root(2),
            vec![FileIntent {
                target: target(),
                source: source(),
                original: PreState::Absent,
            }],
        )
        .unwrap()
}

fn absent() -> PreImage {
    PreImage {
        observation: PreState::Absent,
        backup: None,
    }
}

fn prepared(op: OperationId) -> PreparedStep {
    PreparedStep {
        op,
        ordinal: 0,
        direction: StepDirection::Forward,
        spec: StepSpec {
            target: target(),
            original: absent(),
            pre: PreState::Absent,
            output: StepOutput::Source(source()),
        },
        step_pre: absent(),
        staged: Some((
            StageRef::from_record(op, 0, StepDirection::Forward),
            fingerprint(),
        )),
        retain: BackupRef::from_record(op, 0, StepDirection::Forward, BackupSlot::Displaced),
    }
}

fn applied() -> Evidence {
    Evidence {
        target: Observation::Regular(fingerprint()),
        stage: Artifact::Absent,
        retained: Artifact::Absent,
    }
}

#[test]
fn selected_installation_reports_the_claim_and_the_committed_generation() {
    let dir = fixture_v1();
    let mut writer = open_fixture(&dir);
    let reviewed = plan(&mut writer);
    let op = writer.claim(Claim::Deploy(reviewed)).unwrap();

    let selected = writer.selected_installation().unwrap().unwrap();
    assert_eq!(
        selected
            .pending
            .map(|status| (status.id, status.kind, status.phase)),
        Some((op, OperationKind::Deploy, Phase::Preparing))
    );
    assert_eq!(selected.active_generation, None);

    writer.seal(op, vec![prepared(op)]).unwrap();
    writer
        .set_phase(op, Phase::Prepared, Phase::Applying)
        .unwrap();
    writer
        .record_step(op, 0, StepPhase::Ready, StepPhase::Attempting, None)
        .unwrap();
    writer
        .record_step(
            op,
            0,
            StepPhase::Attempting,
            StepPhase::Applied,
            Some(applied()),
        )
        .unwrap();
    let committed = writer.finish(op, Finalize::DeployCommitted).unwrap();
    let generation = committed.generation.unwrap();

    let selected = writer.selected_installation().unwrap().unwrap();
    assert!(selected.pending.is_none());
    assert_eq!(selected.active_generation, Some(generation.get()));
}
