//! Native copy-adapter witnesses on a qualified synthetic volume.
//!
//! These tests create only disposable installation and library trees under
//! `TMPDIR`. They never touch a real game. Run them with `--ignored` and a
//! `TMPDIR` on tmpfs or Btrfs.

#![cfg(target_os = "linux")]

use std::fs;
use std::path::{Path, PathBuf};

use marduk_core::deployment::{BoundTarget, DeployError, DeploymentInputs, OperationId, PreState};
use marduk_core::ids::{InstalledRevisionId, MemberId, ProfileId, ProfileRevision};
use marduk_core::profiles::{ProfileMember, ProfileSnapshot};
use marduk_core::recovery::{
    BarrierOutcome, Claim, CopyEffects, EffectOutcome, Journal, PreImage, StepDirection,
    StepOutput, StepPhase, StepSpec,
};
use marduk_core::resolve::ResolvedInputs;
use marduk_core::root_identity::{Fingerprint, GameLayout, RootIdentity};
use marduk_core::targets::{RelativePath, RootRole, TargetFile};
use marduk_storage::native_effects::NativeEffects;
use marduk_storage::operation_journal::OperationJournal;
use rusqlite::{Connection, params};
use tempfile::TempDir;

const LAYOUT: GameLayout = GameLayout {
    data: "Data",
    markers: &["SkyrimSE.exe"],
};
const BASE: &[u8] = b"base\n";
const POST: &[u8] = b"b\n";
const ONLY: &[u8] = b"only-a\n";
const EDIT: &[u8] = b"edit\n";
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

    fn target(&self, relative: &str) -> PathBuf {
        self.game.join("Data").join(relative)
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

fn profile() -> ProfileSnapshot {
    ProfileSnapshot {
        id: ProfileId::new(1),
        revision: ProfileRevision::new(0),
        members: vec![ProfileMember {
            id: MemberId::new(1),
            installed: InstalledRevisionId::new(7),
            enabled: true,
        }],
    }
}

fn inputs() -> ResolvedInputs {
    let mut manifests = std::collections::BTreeMap::new();
    manifests.insert(
        InstalledRevisionId::new(7),
        vec![
            TargetFile {
                root: RootRole::Data,
                relative: RelativePath::parse(ONLY_A).unwrap(),
            },
            TargetFile {
                root: RootRole::Data,
                relative: RelativePath::parse(WINNER).unwrap(),
            },
        ],
    );
    ResolvedInputs { manifests }
}

fn locator_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    let mut bytes = vec![0u8];
    bytes.extend_from_slice(path.as_os_str().as_bytes());
    bytes
}

/// The journal's plan intents reference `revision_files`; this test builds the
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
fn recorded_locator_mismatch_is_a_typed_refusal() {
    let fixture = Fixture::new();
    let mut port = fixture.adapter();

    assert_eq!(
        port.verify_recovery_root(&fixture.installation, &fixture.data, None),
        Err(DeployError::TargetUnavailable)
    );

    let other = fixture.temp_path().join("Other Skyrim");
    assert_eq!(
        port.verify_recovery_root(
            &fixture.installation,
            &fixture.data,
            Some(&locator_bytes(&other))
        ),
        Err(DeployError::TargetUnavailable)
    );

    port.verify_recovery_root(
        &fixture.installation,
        &fixture.data,
        Some(&locator_bytes(&fixture.game)),
    )
    .unwrap();
}

#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn unqualified_installation_root_is_refused_before_any_write() {
    let temp = tempfile::tempdir().unwrap();
    let app = temp.path().join("app");
    fs::create_dir_all(&app).unwrap();

    let error = NativeEffects::open(&app, Path::new("/proc"), LAYOUT).unwrap_err();
    assert_eq!(error, DeployError::UnsupportedTarget);
    assert!(!app.join("effects").exists());
}

#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn concurrent_edit_during_apply_conflicts_and_preserves_external_bytes() {
    let fixture = Fixture::new();
    let mut port = fixture.adapter();

    let target = TargetFile {
        root: RootRole::Data,
        relative: RelativePath::parse(WINNER).unwrap(),
    };
    let bound: BoundTarget = port.resolve_target(&target).unwrap();
    let original = match port.observe(&bound) {
        marduk_core::recovery::Observation::Regular(fingerprint) => fingerprint,
        other => panic!("expected a regular original: {other:?}"),
    };
    let source = port
        .source(
            InstalledRevisionId::new(7),
            &RelativePath::parse(WINNER).unwrap(),
        )
        .unwrap();
    let op = OperationId::try_from_u64(1).unwrap();
    let step = port
        .prepare(
            op,
            0,
            StepDirection::Forward,
            &StepSpec {
                target: bound.clone(),
                original: PreImage {
                    observation: PreState::Regular(original.clone()),
                    backup: None,
                },
                pre: PreState::Regular(original.clone()),
                output: StepOutput::Source(source),
            },
        )
        .unwrap();

    fs::write(fixture.target(WINNER), EDIT).unwrap();
    let outcome = port.replace_existing_retain_displaced(
        &bound,
        &original,
        step.staged.as_ref().unwrap().0,
        step.retain,
    );

    assert!(matches!(outcome, EffectOutcome::Conflict(_)), "{outcome:?}");
    assert_eq!(fs::read(fixture.target(WINNER)).unwrap(), EDIT);
    let forward = fixture.effect_dir(op, 0);
    assert_eq!(fs::read(forward.join("original")).unwrap(), BASE);
    assert_eq!(fs::read(forward.join("step")).unwrap(), BASE);
    assert_eq!(fs::read(forward.join("stage")).unwrap(), POST);
    assert!(!forward.join("displaced").exists());
}

#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn restart_after_apply_before_ack_rolls_back_and_never_reapplies() {
    let fixture = Fixture::new();
    let mut journal = fixture.journal();
    let mut port = fixture.adapter();
    let plan = marduk_core::deployment::preview_deployment(
        &mut journal,
        &mut port,
        &profile(),
        &inputs(),
        &fixture.installation,
        &fixture.data,
    )
    .unwrap();
    let op = journal.claim(Claim::Deploy(plan.clone())).unwrap();

    let mut steps = Vec::new();
    for (ordinal, intent) in plan.intents.iter().enumerate() {
        steps.push(
            port.prepare(
                op,
                u32::try_from(ordinal).unwrap(),
                StepDirection::Forward,
                &StepSpec {
                    target: intent.target.clone(),
                    original: PreImage {
                        observation: intent.original.clone(),
                        backup: None,
                    },
                    pre: intent.original.clone(),
                    output: StepOutput::Source(intent.source.clone()),
                },
            )
            .unwrap(),
        );
    }
    journal.seal(op, steps.clone()).unwrap();
    journal
        .set_phase(
            op,
            marduk_core::deployment::Phase::Prepared,
            marduk_core::deployment::Phase::Applying,
        )
        .unwrap();

    let index = plan
        .intents
        .iter()
        .position(|intent| intent.target.target.relative.as_str() == WINNER)
        .unwrap();
    let step = &steps[index];
    journal
        .record_step(
            op,
            step.ordinal,
            StepPhase::Ready,
            StepPhase::Attempting,
            None,
        )
        .unwrap();
    let expected: &Fingerprint = match &step.spec.pre {
        PreState::Regular(fingerprint) => fingerprint,
        PreState::Absent => panic!("winner must exist before the effect"),
    };
    let (stage, _) = step.staged.as_ref().unwrap();
    let outcome =
        port.replace_existing_retain_displaced(&step.spec.target, expected, *stage, step.retain);
    let EffectOutcome::Observed(evidence) = outcome else {
        panic!("expected a durable native effect: {outcome:?}");
    };
    assert!(matches!(
        port.durability_barrier(step, &evidence),
        BarrierOutcome::Durable(_)
    ));
    assert_eq!(fs::read(fixture.target(WINNER)).unwrap(), POST);

    // Crash after the durable effect but before the step records `Applied`.
    drop(port);
    drop(journal);

    let mut journal = OperationJournal::open(&fixture.app).unwrap();
    let mut port = fixture.adapter();
    let status = marduk_core::recovery::recover_pending(
        &mut journal,
        &mut port,
        &fixture.installation,
        &fixture.data,
    )
    .unwrap()
    .unwrap();
    assert_eq!(status.phase, marduk_core::deployment::Phase::RolledBack);
    assert_eq!(fs::read(fixture.target(WINNER)).unwrap(), BASE);
    assert!(!fixture.target(ONLY_A).exists());
    assert_eq!(journal.status(op).unwrap().unwrap().generation, None);

    // A second recovery in the resolved state is a no-op, not a replay.
    assert!(
        marduk_core::recovery::recover_pending(
            &mut journal,
            &mut port,
            &fixture.installation,
            &fixture.data,
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(fs::read(fixture.target(WINNER)).unwrap(), BASE);
    assert!(!fixture.target(ONLY_A).exists());
}
