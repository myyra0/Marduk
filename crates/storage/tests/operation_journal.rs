use std::path::Path;

use marduk_core::deployment::*;
use marduk_core::ids::{InstalledRevisionId, ProfileId, ProfileRevision};
use marduk_core::recovery::*;
use marduk_core::root_identity::{
    FileMetadata, Fingerprint, NativeIdentity, RootError, RootIdentity,
};
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

fn root_state(
    journal: &mut OperationJournal,
    pending: Option<OperationId>,
    active: Option<GenerationId>,
) {
    let state = journal.root_state(&root(2)).unwrap();
    assert_eq!((state.pending, state.active), (pending, active));
}

fn scalar(db: &Connection, sql: &str) -> i64 {
    db.query_row(sql, [], |row| row.get(0)).unwrap()
}

#[test]
fn migrates_v1_catalog_in_place_and_reopens() {
    let dir = fixture_v1();
    drop(open_fixture(&dir));
    let check = Connection::open(dir.path().join(DB)).unwrap();
    assert_eq!(scalar(&check, "PRAGMA user_version"), 2);
    assert_eq!(
        scalar(&check, "SELECT revision FROM profiles WHERE id = 1"),
        7
    );
    assert_eq!(
        scalar(
            &check,
            "SELECT count(*) FROM members WHERE installed_id = 1"
        ),
        1
    );
    assert_eq!(
        scalar(&check, "SELECT count(*) FROM pragma_foreign_key_check"),
        0
    );
    for table in [
        "installations",
        "deployment_plans",
        "plan_intents",
        "operations",
        "operation_steps",
        "generations",
        "root_leases",
    ] {
        assert_eq!(
            check
                .query_row(
                    "SELECT count(*) FROM sqlite_schema WHERE type = 'table' AND name = ?1",
                    [table],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap(),
            1,
            "missing v2 table {table}"
        );
    }
    let mut reopened = OperationJournal::open(dir.path()).unwrap();
    root_state(&mut reopened, None, None);
    let saved = plan(&mut reopened);
    let mut other = OperationJournal::open(dir.path()).unwrap();
    let read = other.plan(saved.id).unwrap().unwrap();
    assert_eq!(
        (read.id, read.profile, read.revision),
        (saved.id, ProfileId::new(1), ProfileRevision::new(7))
    );
    assert_eq!((read.installation, read.data), (root(1), root(2)));
    let [intent] = read.intents.as_slice() else {
        panic!("one exact intent required");
    };
    assert_eq!(intent.target.target.root, RootRole::Data);
    assert_eq!(intent.target.target.relative.as_str(), FILE);
    assert_eq!(
        (intent.target.parent.clone(), intent.target.leaf.as_str()),
        (root(3), "a-only.dds")
    );
    assert_eq!(intent.source.installed, InstalledRevisionId::new(1));
    assert_eq!(intent.source.path.as_str(), FILE);
    assert_eq!(
        (intent.source.len, intent.source.sha256),
        (7, source_hash())
    );
    assert!(matches!(intent.original, PreState::Absent));
}

#[test]
fn claim_seal_and_attempt_survive_reopen_and_exclude_second_client() {
    let dir = fixture_v1();
    let mut first = open_fixture(&dir);
    let a = plan(&mut first);
    let mut second = OperationJournal::open(dir.path()).unwrap();
    let b = plan(&mut second);
    let op = first.claim(Claim::Deploy(a.clone())).unwrap();
    root_state(&mut second, Some(op), None);
    let status = second.claimed(a.id).unwrap().unwrap();
    assert_eq!(
        (
            status.id,
            status.kind,
            status.phase,
            status.generation,
            status.blocker
        ),
        (op, OperationKind::Deploy, Phase::Preparing, None, None)
    );
    assert!(matches!(
        second.claim(Claim::Deploy(b.clone())),
        Err(DeployError::Busy)
    ));
    assert!(second.claimed(b.id).unwrap().is_none());
    first.seal(op, vec![prepared(op)]).unwrap();
    assert_eq!(second.status(op).unwrap().unwrap().phase, Phase::Prepared);
    first
        .set_phase(op, Phase::Prepared, Phase::Applying)
        .unwrap();
    first
        .record_step(op, 0, StepPhase::Ready, StepPhase::Attempting, None)
        .unwrap();
    drop(first);
    drop(second);

    let mut reopened = OperationJournal::open(dir.path()).unwrap();
    root_state(&mut reopened, Some(op), None);
    assert_eq!(
        reopened.claimed(a.id).unwrap().unwrap().phase,
        Phase::Applying
    );
    assert!(reopened.finish(op, Finalize::DeployCommitted).is_err());
    assert!(
        reopened
            .record_step(op, 0, StepPhase::Ready, StepPhase::Applied, Some(applied()))
            .is_err()
    );
    let steps = reopened.steps(op).unwrap();
    let [row] = steps.as_slice() else {
        panic!("invalid persisted cardinality");
    };
    assert_eq!(row.phase, StepPhase::Attempting);
    assert!(row.evidence.is_none() && row.undo.is_none());
    assert_eq!(
        row.prepared.staged.as_ref().unwrap().0,
        StageRef::from_record(op, 0, StepDirection::Forward)
    );
    assert_eq!(row.prepared.staged.as_ref().unwrap().1, fingerprint());
    assert_eq!(
        row.prepared.retain,
        BackupRef::from_record(op, 0, StepDirection::Forward, BackupSlot::Displaced)
    );
    assert!(matches!(
        row.prepared.step_pre.observation,
        PreState::Absent
    ));
    assert!(row.prepared.step_pre.backup.is_none());
    root_state(&mut reopened, Some(op), None);
}

#[test]
fn lost_sql_ack_reopens_committed_claim_without_second_effect() {
    let dir = fixture_v1();
    let mut writer = open_fixture(&dir);
    let reviewed = plan(&mut writer);
    let id = reviewed.id;
    let op = writer.claim(Claim::Deploy(reviewed)).unwrap();
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

    let witness = Connection::open(dir.path().join(DB)).unwrap();
    root_state(&mut writer, Some(op), None);
    assert_eq!(scalar(&witness, "SELECT count(*) FROM generations"), 0);
    // Commit on the real connection and deliberately drop, rather than use,
    // the result. No failure is simulated inside SQLite.
    let _lost_ack = writer.finish(op, Finalize::DeployCommitted).unwrap();
    drop(writer);
    witness
        .execute("UPDATE profiles SET revision = 8 WHERE id = 1", [])
        .unwrap();

    let mut reopened = OperationJournal::open(dir.path()).unwrap();
    let mut effects = NeverApply;
    let replayed = commit_deployment(&mut reopened, &mut effects, id, || {
        panic!("replayed a pre-public phase")
    })
    .unwrap();
    assert_eq!(
        (replayed.id, replayed.kind, replayed.phase, replayed.blocker),
        (op, OperationKind::Deploy, Phase::Completed, None)
    );
    let generation = replayed.generation.unwrap();
    root_state(&mut reopened, None, Some(generation));
    let record = reopened.generation(generation).unwrap();
    assert_eq!((record.id, record.deploy), (generation, op));
    let [row] = record.steps.as_slice() else {
        panic!("one immutable generation step required");
    };
    assert_eq!(row.phase, StepPhase::Applied);
    let evidence = row.evidence.as_ref().unwrap();
    assert!(matches!(&evidence.target, Observation::Regular(file) if *file == fingerprint()));
    assert!(matches!(evidence.stage, Artifact::Absent));
    assert!(matches!(evidence.retained, Artifact::Absent));
    assert_eq!(
        scalar(
            &witness,
            "SELECT count(*) FROM operations WHERE phase = 'completed'"
        ),
        1
    );
    assert_eq!(scalar(&witness, "SELECT count(*) FROM operations"), 1);
    assert_eq!(scalar(&witness, "SELECT count(*) FROM operation_steps"), 1);
    assert_eq!(scalar(&witness, "SELECT count(*) FROM generations"), 1);
    assert_eq!(
        scalar(
            &witness,
            "SELECT count(*) FROM root_leases WHERE pending_op IS NULL AND active_generation IS NOT NULL"
        ),
        1
    );
}

/// A replay must not reopen targets, inspect source paths, or invoke effects.
struct NeverApply;

impl DeploymentInputs for NeverApply {
    type RootGuard = ();
    type LibraryGuard = ();
    fn try_root_gate(&mut self) -> Result<(), DeployError> {
        panic!("acquired a new root gate on replay")
    }
    fn try_library_gate(&mut self) -> Result<(), DeployError> {
        panic!("locked the library on replay")
    }
    fn verify_root(&mut self, _: &RootIdentity, _: &RootIdentity) -> Result<(), DeployError> {
        panic!("checked current root on replay")
    }
    fn resolve_target(&mut self, _: &TargetFile) -> Result<BoundTarget, RootError> {
        panic!("resolved a target on replay")
    }
    fn profile_revision(&mut self, _: ProfileId) -> Result<ProfileRevision, DeployError> {
        panic!("checked current revision on replay")
    }
    fn source(
        &mut self,
        _: InstalledRevisionId,
        _: &RelativePath,
    ) -> Result<SourceRef, DeployError> {
        panic!("read a source on replay")
    }
}

impl CopyEffects for NeverApply {
    fn observe(&mut self, _: &BoundTarget) -> Observation {
        panic!("observed target on replay")
    }
    fn prepare(
        &mut self,
        _: OperationId,
        _: u32,
        _: StepDirection,
        _: &StepSpec,
    ) -> Result<PreparedStep, DeployError> {
        panic!("prepared a new effect on replay")
    }
    fn publish_new_absent(&mut self, _: &BoundTarget, _: StageRef) -> EffectOutcome {
        panic!("published on replay")
    }
    fn replace_existing_retain_displaced(
        &mut self,
        _: &BoundTarget,
        _: &Fingerprint,
        _: StageRef,
        _: BackupRef,
    ) -> EffectOutcome {
        panic!("replaced on replay")
    }
    fn detach_existing_retain(
        &mut self,
        _: &BoundTarget,
        _: &Fingerprint,
        _: BackupRef,
    ) -> EffectOutcome {
        panic!("detached on replay")
    }
    fn inspect_step(&mut self, _: &PreparedStep) -> Evidence {
        panic!("inspected effect on replay")
    }
    fn durability_barrier(&mut self, _: &PreparedStep, _: &Evidence) -> BarrierOutcome {
        panic!("ran effect barrier on replay")
    }
    fn validate_backup(&mut self, _: BackupRef, _: &Fingerprint) -> Result<(), DeployError> {
        panic!("read a backup on replay")
    }
}
