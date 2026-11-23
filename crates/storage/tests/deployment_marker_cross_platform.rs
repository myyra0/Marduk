//! Cross-platform marker and prepare witnesses on disposable qualified roots.

use std::fs;
use std::path::{Path, PathBuf};

use marduk_core::deployment::{DeployError, DeploymentInputs, FileIntent, OperationId, PreState};
use marduk_core::ids::{InstalledRevisionId, ProfileId, ProfileRevision};
use marduk_core::recovery::{
    Claim, CopyEffects, Journal, PreImage, StepDirection, StepOutput, StepSpec,
};
use marduk_core::root_identity::RootIdentity;
use marduk_core::targets::{RelativePath, RootRole, TargetFile};
use marduk_games::skyrim_se::LAYOUT;
use marduk_platform::{
    DEPLOYMENT_MARKER, DeploymentOwner, InstallationBinding, reopen_installation,
};
use marduk_storage::Catalog;
use marduk_storage::native_effects::NativeEffects;
use marduk_storage::operation_journal::OperationJournal;
use rusqlite::{Connection, params};
use tempfile::TempDir;

const BASE: &[u8] = b"base\n";
const POST: &[u8] = b"b\n";
const ONLY: &[u8] = b"only-a\n";
const WINNER: &str = "textures/winner.dds";
const ONLY_A: &str = "textures/a-only.dds";

struct Fixture {
    temp: TempDir,
    app: PathBuf,
    game: PathBuf,
    installation: RootIdentity,
    data: RootIdentity,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        assert!(
            marduk_platform::effects::volume_qualified(temp.path()),
            "this witness needs a qualified volume; point TMPDIR at tmpfs or Btrfs"
        );
        let app = temp.path().join("app");
        fs::create_dir_all(&app).unwrap();
        let game = temp.path().join("Skyrim Special Edition");
        fs::create_dir_all(game.join("Data/textures")).unwrap();
        fs::write(game.join("SkyrimSE.exe"), b"").unwrap();
        fs::write(game.join("Data/textures/winner.dds"), BASE).unwrap();
        fs::write(game.join("Data/textures/untouched.txt"), b"keep\n").unwrap();

        let binding = marduk_platform::bind_installation(&game, &LAYOUT).unwrap();
        let (installation, data) = binding.identities();
        let installation = installation.clone();
        let data = data.clone();
        drop(binding);

        let payload = app.join("revisions/7/payload/textures");
        fs::create_dir_all(&payload).unwrap();
        fs::write(payload.join("winner.dds"), POST).unwrap();
        fs::write(payload.join("a-only.dds"), ONLY).unwrap();

        Self {
            temp,
            app,
            game,
            installation,
            data,
        }
    }

    fn journal(&self) -> OperationJournal {
        let mut journal = OperationJournal::open(&self.app).unwrap();
        seed_revision_files(&self.app);
        journal
            .select_verified_installation(&self.game, &self.installation, &self.data)
            .unwrap();
        journal
    }

    fn adapter(&self) -> NativeEffects {
        NativeEffects::open(&self.app, &self.game, LAYOUT).unwrap()
    }

    fn binding(&self) -> InstallationBinding {
        reopen_installation(&self.game, &LAYOUT, &self.installation, &self.data).unwrap()
    }

    fn effect_dir(&self, op: OperationId, ordinal: u32) -> PathBuf {
        self.app
            .join("effects/ops")
            .join(op.get().to_string())
            .join(ordinal.to_string())
            .join("forward")
    }

    fn temp_path(&self) -> &Path {
        self.temp.path()
    }
}

/// The journal's plan intents reference `revision_files`; these tests build the
/// payload directly, so record the same rows the catalog would publish.
fn seed_revision_files(app: &Path) {
    let connection = Connection::open(app.join("catalog.sqlite3")).unwrap();
    connection
        .execute_batch("PRAGMA foreign_keys = ON;")
        .unwrap();
    connection
        .execute(
            "INSERT OR IGNORE INTO imports
             (id, profile_id, expected_revision, target, display_name, state, owner, archive_sha, result_revision)
             VALUES (7, 1, 0, 'append', 'fixture', 'committed', x'01', zeroblob(32), 1)",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT OR IGNORE INTO installed_revisions (id) VALUES (7)",
            [],
        )
        .unwrap();
    for (ordinal, relative) in [ONLY_A, WINNER].iter().enumerate() {
        let fingerprint = marduk_platform::effects::fingerprint_path(
            &app.join("revisions/7/payload").join(relative),
        )
        .unwrap();
        connection
            .execute(
                "INSERT OR IGNORE INTO revision_files
                 (revision_id, ordinal, role, path, len, sha)
                 VALUES (7, ?1, 'data', ?2, ?3, ?4)",
                params![
                    i64::try_from(ordinal).unwrap(),
                    relative,
                    i64::try_from(fingerprint.len).unwrap(),
                    fingerprint.sha256.as_slice()
                ],
            )
            .unwrap();
    }
}

#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn incomplete_marker_is_a_conflict_and_is_not_replaced() {
    let fixture = Fixture::new();
    let marker = fixture.game.join(DEPLOYMENT_MARKER);
    let binding = fixture.binding();
    let id = Catalog::open(&fixture.app).unwrap().id();
    #[cfg(unix)]
    let path_only = {
        use std::os::unix::ffi::OsStrExt;
        [fixture.app.as_os_str().as_bytes(), &[0]].concat()
    };
    #[cfg(windows)]
    let path_only: Vec<u8> = {
        use std::os::windows::ffi::OsStrExt;
        fixture
            .app
            .as_os_str()
            .encode_wide()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect()
    };
    for incomplete in [
        b"".as_slice(),
        fixture.app.to_str().unwrap().as_bytes(),
        &path_only,
    ] {
        fs::write(&marker, incomplete).unwrap();
        assert_eq!(
            binding.deployment_owner(id),
            Err(DeployError::RecoveryConflict)
        );
        assert_eq!(
            binding.claim_deployment(id, &fixture.app),
            Err(DeployError::RecoveryConflict)
        );
        assert_eq!(
            binding.clear_deployment(id),
            Err(DeployError::RecoveryConflict)
        );
        assert_eq!(fs::read(&marker).unwrap(), incomplete);
    }
}

#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn deployment_marker_reads_back_through_a_fresh_binding() {
    let fixture = Fixture::new();
    let id = Catalog::open(&fixture.app).unwrap().id();
    assert_eq!(
        fixture
            .binding()
            .claim_deployment(id, &fixture.app)
            .unwrap(),
        DeploymentOwner::Owned(fixture.app.clone())
    );
    let marker = fixture.game.join(DEPLOYMENT_MARKER);
    let original_record = fs::read(&marker).unwrap();

    let binding = fixture.binding();
    assert_eq!(
        binding
            .deployment_owner(Catalog::open(&fixture.app).unwrap().id())
            .unwrap(),
        DeploymentOwner::Owned(fixture.app.clone()),
        "a fresh binding reads the published marker"
    );

    let other = fixture.temp_path().join("app-other");
    let other_id = Catalog::open(&other).unwrap().id();
    assert_eq!(
        binding.claim_deployment(other_id, &other).unwrap(),
        DeploymentOwner::Foreign(fixture.app.clone())
    );
    assert_eq!(
        fs::read(&marker).unwrap(),
        original_record,
        "the foreign claim left A's marker intact"
    );
    assert_eq!(
        binding.clear_deployment(other_id),
        Err(DeployError::ActiveDeployment)
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(&marker, fs::Permissions::from_mode(0o000)).unwrap();
        assert_eq!(
            binding.deployment_owner(id),
            Err(DeployError::RecoveryConflict),
            "an unreadable marker is a conflict, never vacant"
        );
        assert_eq!(
            binding.claim_deployment(other_id, &other),
            Err(DeployError::RecoveryConflict)
        );
        fs::set_permissions(&marker, fs::Permissions::from_mode(0o600)).unwrap();
    }

    binding.clear_deployment(id).unwrap();
    assert_eq!(
        binding.deployment_owner(id).unwrap(),
        DeploymentOwner::Vacant
    );
    assert!(!marker.exists());
}

#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn simultaneous_catalog_claims_publish_one_complete_owner() {
    let fixture = Fixture::new();
    let other = fixture.temp_path().join("app-other");
    let a_id = Catalog::open(&fixture.app).unwrap().id();
    let b_id = Catalog::open(&other).unwrap().id();
    let barrier = std::sync::Barrier::new(2);
    let a_binding = fixture.binding();
    let b_binding = fixture.binding();
    let claim = |binding: &InstallationBinding, catalog: &Path, id| {
        barrier.wait();
        binding.claim_deployment(id, catalog).unwrap()
    };
    let (a, b) = std::thread::scope(|scope| {
        let a = scope.spawn(|| claim(&a_binding, &fixture.app, a_id));
        let b = scope.spawn(|| claim(&b_binding, &other, b_id));
        (a.join().unwrap(), b.join().unwrap())
    });
    let (DeploymentOwner::Owned(winner), DeploymentOwner::Foreign(loser_saw)) =
        (if matches!(a, DeploymentOwner::Owned(_)) {
            (a, b)
        } else {
            (b, a)
        })
    else {
        panic!("expected one winner and one foreign claim");
    };
    assert_eq!(winner, loser_saw);
    assert_eq!(
        fixture
            .binding()
            .deployment_owner(Catalog::open(&winner).unwrap().id())
            .unwrap(),
        DeploymentOwner::Owned(winner)
    );
    assert_eq!(
        fs::read_dir(&fixture.game)
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(DEPLOYMENT_MARKER)
            })
            .count(),
        1
    );
}

#[test]
#[ignore = "requires a qualified test volume"]
fn unrecorded_slots_are_quarantined_but_recorded_slots_refuse_prepare() {
    let fixture = Fixture::new();
    let mut journal = fixture.journal();
    let mut port = fixture.adapter();
    let winner = RelativePath::parse(WINNER).unwrap();
    let bound = port
        .resolve_target(&TargetFile {
            root: RootRole::Data,
            relative: winner.clone(),
        })
        .unwrap();
    let expected = match port.observe(&bound) {
        marduk_core::recovery::Observation::Regular(fingerprint) => fingerprint,
        other => panic!("expected a regular original: {other:?}"),
    };
    let source = port.source(InstalledRevisionId::new(7), &winner).unwrap();
    let plan = journal
        .save_plan(
            ProfileId::new(1),
            ProfileRevision::new(0),
            fixture.installation.clone(),
            fixture.data.clone(),
            vec![FileIntent {
                target: bound.clone(),
                source: source.clone(),
                original: PreState::Regular(expected.clone()),
            }],
        )
        .unwrap();
    let op = journal.claim(Claim::Deploy(plan)).unwrap();
    let spec = StepSpec {
        target: bound,
        original: PreImage {
            observation: PreState::Regular(expected.clone()),
            backup: None,
        },
        pre: PreState::Regular(expected),
        output: StepOutput::Source(source),
    };
    let directory = fixture.effect_dir(op, 0);
    port.prepare(op, 0, StepDirection::Forward, &spec).unwrap();
    fs::write(directory.join("stage.quarantine-0"), b"older evidence").unwrap();
    let prepared = port.prepare(op, 0, StepDirection::Forward, &spec).unwrap();
    assert_eq!(
        fs::read(directory.join("stage.quarantine-0")).unwrap(),
        b"older evidence"
    );
    assert_eq!(
        fs::read(directory.join("stage.quarantine-1")).unwrap(),
        POST
    );
    assert_eq!(
        fs::read(directory.join("original.quarantine-0")).unwrap(),
        BASE
    );
    assert_eq!(fs::read(directory.join("step.quarantine-0")).unwrap(), BASE);
    assert_eq!(fs::read(directory.join("stage")).unwrap(), POST);
    journal.seal(op, vec![prepared]).unwrap();
    assert_eq!(
        port.prepare(op, 0, StepDirection::Forward, &spec)
            .unwrap_err(),
        DeployError::RecoveryConflict
    );
    assert_eq!(
        fs::read(directory.join("original")).unwrap(),
        BASE,
        "the recorded backup is intact"
    );
    assert!(!directory.join("stage.quarantine-2").exists());
}
