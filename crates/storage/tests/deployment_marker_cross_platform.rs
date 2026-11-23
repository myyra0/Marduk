//! Cross-platform witnesses for the deployment marker and prepare's refusal to
//! adopt an existing staged step.
//!
//! These two behaviours are otherwise only witnessed by
//! `native_effects.rs`, which is Linux-gated. They create only disposable
//! installation and library trees under `TMPDIR`. Run them with `--ignored` and
//! a `TMPDIR` on tmpfs or Btrfs (or an NTFS volume on Windows).

use std::fs;
use std::path::{Path, PathBuf};

use marduk_core::deployment::{DeployError, DeploymentInputs, OperationId, PreState};
use marduk_core::ids::InstalledRevisionId;
use marduk_core::recovery::{CopyEffects, PreImage, StepDirection, StepOutput, StepSpec};
use marduk_core::root_identity::RootIdentity;
use marduk_core::targets::{RelativePath, RootRole, TargetFile};
use marduk_games::skyrim_se::LAYOUT;
use marduk_storage::native_effects::{DEPLOYMENT_MARKER, DeploymentOwner, NativeEffects};
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
fn deployment_marker_reads_back_through_a_fresh_adapter() {
    let fixture = Fixture::new();
    fixture.adapter().claim_deployment(&fixture.app).unwrap();
    let marker = fixture.game.join(DEPLOYMENT_MARKER);

    let port = fixture.adapter();
    assert_eq!(
        port.deployment_owner().unwrap(),
        DeploymentOwner::Owned(fixture.app.clone()),
        "a fresh adapter reads the marker left by the previous process"
    );

    let other = fixture.temp_path().join("app-other");
    fs::create_dir(&other).unwrap();
    let foreign = NativeEffects::open(&other, &fixture.game, LAYOUT).unwrap();
    assert_eq!(
        foreign.claim_deployment(&other),
        Err(DeployError::ActiveDeployment)
    );
    assert_eq!(
        fs::read_to_string(&marker).unwrap(),
        fixture.app.to_string_lossy(),
        "the foreign claim left A's marker intact"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(&marker, fs::Permissions::from_mode(0o000)).unwrap();
        assert_eq!(
            port.deployment_owner(),
            Err(DeployError::RecoveryConflict),
            "an unreadable marker is a conflict, never vacant"
        );
        assert_eq!(
            foreign.claim_deployment(&other),
            Err(DeployError::RecoveryConflict)
        );
        fs::set_permissions(&marker, fs::Permissions::from_mode(0o600)).unwrap();
    }

    port.clear_deployment().unwrap();
    assert_eq!(port.deployment_owner().unwrap(), DeploymentOwner::Vacant);
    assert!(!marker.exists());
}

#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn a_second_prepare_cannot_adopt_a_matching_stage_or_backup() {
    let fixture = Fixture::new();
    drop(fixture.journal());
    let mut port = fixture.adapter();
    let op = OperationId::try_from_u64(1).unwrap();

    // A stage this adapter already wrote is refused, not adopted, by the next
    // prepare for the same step.
    let only = RelativePath::parse(ONLY_A).unwrap();
    let bound = port
        .resolve_target(&TargetFile {
            root: RootRole::Data,
            relative: only.clone(),
        })
        .unwrap();
    let spec = StepSpec {
        target: bound,
        original: PreImage {
            observation: PreState::Absent,
            backup: None,
        },
        pre: PreState::Absent,
        output: StepOutput::Source(port.source(InstalledRevisionId::new(7), &only).unwrap()),
    };
    port.prepare(op, 0, StepDirection::Forward, &spec).unwrap();
    let stage = fixture.effect_dir(op, 0).join("stage");
    assert_eq!(fs::read(&stage).unwrap(), ONLY);
    assert_eq!(
        port.prepare(op, 0, StepDirection::Forward, &spec)
            .unwrap_err(),
        DeployError::RecoveryConflict
    );
    assert_eq!(
        fs::read(&stage).unwrap(),
        ONLY,
        "the refused stage is intact"
    );

    // A matching pre-existing backup is likewise refused.
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
    let spec = StepSpec {
        target: bound,
        original: PreImage {
            observation: PreState::Regular(expected.clone()),
            backup: None,
        },
        pre: PreState::Regular(expected),
        output: StepOutput::Source(port.source(InstalledRevisionId::new(7), &winner).unwrap()),
    };
    let op = OperationId::try_from_u64(2).unwrap();
    let directory = fixture.effect_dir(op, 0);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("original"), BASE).unwrap();
    assert_eq!(
        port.prepare(op, 0, StepDirection::Forward, &spec)
            .unwrap_err(),
        DeployError::RecoveryConflict
    );
    assert_eq!(
        fs::read(directory.join("original")).unwrap(),
        BASE,
        "the refused backup is intact"
    );
}
