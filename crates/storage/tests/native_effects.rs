//! Native copy-adapter witnesses on a qualified synthetic volume.
//!
//! These tests create only disposable installation and library trees under
//! `TMPDIR`. They never touch a real game. Run them with `--ignored` and a
//! `TMPDIR` on tmpfs or Btrfs.

#![cfg(target_os = "linux")]

use std::fs;
use std::path::{Path, PathBuf};

use marduk_core::deployment::{
    BoundTarget, DeployError, DeploymentInputs, OperationId, PreState, commit_deployment,
};
use marduk_core::ids::{InstalledRevisionId, MemberId, ProfileId, ProfileRevision};
use marduk_core::profiles::{ProfileMember, ProfileSnapshot};
use marduk_core::recovery::{
    BarrierOutcome, Claim, CopyEffects, EffectOutcome, Journal, PreImage, StepDirection,
    StepOutput, StepPhase, StepSpec,
};
use marduk_core::resolve::ResolvedInputs;
use marduk_core::root_identity::{Fingerprint, GameLayout, RootError, RootIdentity};
use marduk_core::targets::{RelativePath, RootRole, TargetFile};
use marduk_storage::native_effects::{NativeEffects, ProofEvent};
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

fn inputs_three() -> ResolvedInputs {
    let mut inputs = inputs();
    inputs
        .manifests
        .get_mut(&InstalledRevisionId::new(7))
        .unwrap()
        .insert(
            0,
            TargetFile {
                root: RootRole::Data,
                relative: RelativePath::parse("textures/second.dds").unwrap(),
            },
        );
    inputs
}

fn restore_spec(forward: &marduk_core::recovery::PreparedStep) -> StepSpec {
    let original = forward.spec.original.clone();
    let output = match &original.observation {
        PreState::Absent => StepOutput::Absent,
        PreState::Regular(_) => {
            StepOutput::Original(original.backup.expect("original backup recorded"))
        }
    };
    StepSpec {
        target: forward.spec.target.clone(),
        original,
        pre: match &forward.staged {
            Some((_, fingerprint)) => PreState::Regular(fingerprint.clone()),
            None => PreState::Absent,
        },
        output,
    }
}

fn undo_spec(forward: &marduk_core::recovery::PreparedStep) -> StepSpec {
    let output = match &forward.spec.pre {
        PreState::Absent => StepOutput::Absent,
        PreState::Regular(_) => {
            StepOutput::Original(forward.step_pre.backup.expect("step backup recorded"))
        }
    };
    StepSpec {
        target: forward.spec.target.clone(),
        original: forward.spec.original.clone(),
        pre: match &forward.staged {
            Some((_, fingerprint)) => PreState::Regular(fingerprint.clone()),
            None => PreState::Absent,
        },
        output,
    }
}

fn execute_forward(port: &mut NativeEffects, step: &marduk_core::recovery::PreparedStep) {
    let outcome = match (&step.spec.pre, &step.spec.output, step.staged.as_ref()) {
        (PreState::Regular(expected), _, Some((stage, _))) => {
            port.replace_existing_retain_displaced(&step.spec.target, expected, *stage, step.retain)
        }
        (PreState::Regular(expected), StepOutput::Absent, _) => {
            port.detach_existing_retain(&step.spec.target, expected, step.retain)
        }
        other => panic!("unexpected forward restore shape: {other:?}"),
    };
    assert!(matches!(outcome, EffectOutcome::Observed(_)), "{outcome:?}");
}

#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn restart_after_record_undo_uses_the_sealed_stage_fingerprint() {
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
    let forward = &steps[index];
    journal
        .record_step(
            op,
            forward.ordinal,
            StepPhase::Ready,
            StepPhase::Attempting,
            None,
        )
        .unwrap();
    execute_forward(&mut port, forward);
    journal
        .set_phase(
            op,
            marduk_core::deployment::Phase::Applying,
            marduk_core::deployment::Phase::RollingBack,
        )
        .unwrap();

    // Core prepares the rollback step and records it before the cut.
    let undo = port
        .prepare(
            op,
            forward.ordinal,
            StepDirection::Undo,
            &undo_spec(forward),
        )
        .unwrap();
    journal
        .record_undo(op, forward.ordinal, undo.clone())
        .unwrap();
    assert_eq!(fs::read(fixture.target(WINNER)).unwrap(), POST);

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
    let rows = journal.steps(op).unwrap();
    assert_eq!(rows[index].undo.as_ref(), Some(&undo));
    assert_eq!(journal.root_state(&fixture.data).unwrap().pending, None);
}

#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn restart_mid_restore_continues_in_reverse_order() {
    let fixture = Fixture::new();
    fs::write(fixture.target("second.dds"), b"second\n").unwrap();
    fs::create_dir_all(fixture.app.join("revisions/7/payload/textures")).unwrap();
    fs::write(
        fixture.app.join("revisions/7/payload/textures/second.dds"),
        b"second\n",
    )
    .unwrap();
    let mut journal = fixture.journal();
    {
        let connection = Connection::open(fixture.app.join("catalog.sqlite3")).unwrap();
        let fingerprint = marduk_platform::effects::fingerprint_path(
            &fixture.app.join("revisions/7/payload/textures/second.dds"),
        )
        .unwrap();
        connection
            .execute(
                "INSERT OR IGNORE INTO revision_files
                 (revision_id, ordinal, role, path, len, sha)
                 VALUES (7, 2, 'data', 'textures/second.dds', ?1, ?2)",
                params![
                    i64::try_from(fingerprint.len).unwrap(),
                    fingerprint.sha256.as_slice()
                ],
            )
            .unwrap();
    }
    let mut port = fixture.adapter();
    let plan = marduk_core::deployment::preview_deployment(
        &mut journal,
        &mut port,
        &profile(),
        &inputs_three(),
        &fixture.installation,
        &fixture.data,
    )
    .unwrap();
    let status =
        marduk_core::deployment::commit_deployment(&mut journal, &mut port, plan.id, || false)
            .unwrap();
    assert_eq!(status.phase, marduk_core::deployment::Phase::Completed);
    let generation = status.generation.expect("committed generation");
    assert_eq!(fs::read(fixture.target(WINNER)).unwrap(), POST);
    assert_eq!(fs::read(fixture.target("second.dds")).unwrap(), b"second\n");

    // Start Restore and record every reverse step, then apply the first one
    // (highest ordinal) without acknowledging it before the cut.
    let record = journal.generation(generation).unwrap();
    let restore = journal
        .claim(Claim::Restore {
            installation: fixture.installation.clone(),
            data: fixture.data.clone(),
            generation,
        })
        .unwrap();
    let mut steps = Vec::new();
    for row in record.steps.iter().rev() {
        steps.push(
            port.prepare(
                restore,
                row.prepared.ordinal,
                StepDirection::Forward,
                &restore_spec(&row.prepared),
            )
            .unwrap(),
        );
    }
    journal.seal(restore, steps.clone()).unwrap();
    journal
        .set_phase(
            restore,
            marduk_core::deployment::Phase::Prepared,
            marduk_core::deployment::Phase::Restoring,
        )
        .unwrap();
    let first = &steps[0];
    assert!(
        matches!(first.spec.pre, PreState::Regular(_)),
        "the first restore step must exercise an exchange"
    );
    journal
        .record_step(
            restore,
            first.ordinal,
            StepPhase::Ready,
            StepPhase::Attempting,
            None,
        )
        .unwrap();
    execute_forward(&mut port, first);
    assert_eq!(fs::read(fixture.target(WINNER)).unwrap(), BASE);

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
    assert_eq!(status.phase, marduk_core::deployment::Phase::Completed);
    assert_eq!(fs::read(fixture.target(WINNER)).unwrap(), BASE);
    assert_eq!(fs::read(fixture.target("second.dds")).unwrap(), b"second\n");
    assert!(!fixture.target(ONLY_A).exists());
    assert_eq!(
        fs::read(fixture.target("textures/untouched.txt")).unwrap(),
        b"keep\n"
    );
    assert_eq!(journal.root_state(&fixture.data).unwrap().pending, None);
    assert_eq!(journal.root_state(&fixture.data).unwrap().active, None);
}

#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn unreadable_private_slot_is_unavailable_not_absent() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new();
    let mut port = fixture.adapter();
    let target = TargetFile {
        root: RootRole::Data,
        relative: RelativePath::parse(ONLY_A).unwrap(),
    };
    let bound = port.resolve_target(&target).unwrap();
    assert_eq!(
        port.observe(&bound),
        marduk_core::recovery::Observation::Absent
    );
    let source = port
        .source(
            InstalledRevisionId::new(7),
            &RelativePath::parse(ONLY_A).unwrap(),
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
                    observation: PreState::Absent,
                    backup: None,
                },
                pre: PreState::Absent,
                output: StepOutput::Source(source),
            },
        )
        .unwrap();

    let stage = fixture.effect_dir(op, 0).join("stage");
    fs::set_permissions(&stage, fs::Permissions::from_mode(0o000)).unwrap();
    let evidence = port.inspect_step(&step);
    fs::set_permissions(&stage, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        evidence.stage,
        marduk_core::recovery::Artifact::Unavailable,
        "an unreadable stage is unavailable evidence, not an empty slot"
    );
}

#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn prepared_private_artifacts_are_durable_and_unpublished() {
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

    // Prepare must return with the private evidence readable by a fresh port
    // and no public effect; the cut here is before any rename.
    drop(port);
    let mut port = fixture.adapter();
    for step in &steps {
        let evidence = port.inspect_step(step);
        assert_eq!(
            evidence.target,
            match &step.spec.pre {
                PreState::Absent => marduk_core::recovery::Observation::Absent,
                PreState::Regular(fingerprint) =>
                    marduk_core::recovery::Observation::Regular(fingerprint.clone()),
            }
        );
        assert_eq!(
            evidence.stage,
            marduk_core::recovery::Artifact::Regular(step.staged.as_ref().unwrap().1.clone())
        );
    }
    assert_eq!(fs::read(fixture.target(WINNER)).unwrap(), BASE);
    assert!(!fixture.target(ONLY_A).exists());
    let status = marduk_core::recovery::recover_pending(
        &mut journal,
        &mut port,
        &fixture.installation,
        &fixture.data,
    )
    .unwrap()
    .unwrap();
    assert_eq!(status.phase, marduk_core::deployment::Phase::Cancelled);
    assert_eq!(fs::read(fixture.target(WINNER)).unwrap(), BASE);
}

#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn private_chain_is_synced_before_the_first_public_effect() {
    let fixture = Fixture::new();
    let mut journal = fixture.journal();
    let mut port = fixture.adapter();
    port.proof_record_events();
    let plan = marduk_core::deployment::preview_deployment(
        &mut journal,
        &mut port,
        &profile(),
        &inputs(),
        &fixture.installation,
        &fixture.data,
    )
    .unwrap();
    let status = commit_deployment(&mut journal, &mut port, plan.id, || false).unwrap();
    assert_eq!(status.phase, marduk_core::deployment::Phase::Completed);

    let events = port.proof_events().expect("recording was enabled");
    let first_effect = events
        .iter()
        .position(|event| matches!(event, ProofEvent::PublicEffect))
        .expect("the commit publishes at least one file");
    let before = &events[..first_effect];
    let app = fs::canonicalize(&fixture.app).unwrap();
    for ordinal in 0..plan.intents.len() {
        let directory = fixture.effect_dir(status.id, u32::try_from(ordinal).unwrap());
        let mut current = Some(directory.as_path());
        while let Some(path) = current {
            let resolved = fs::canonicalize(path).unwrap();
            assert!(
                before.iter().any(
                    |event| matches!(event, ProofEvent::Synced(synced) if *synced == resolved)
                ),
                "{} was not synced before the first public effect",
                path.display()
            );
            if resolved == app {
                break;
            }
            current = path.parent();
        }
    }
    // The chain never crosses the app-owned root.
    for event in events {
        if let ProofEvent::Synced(path) = event {
            assert!(
                path.starts_with(&app),
                "synced outside the app root: {}",
                path.display()
            );
        }
    }
}

#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn sync_chain_stops_at_the_resolved_root_for_a_symlinked_library_spelling() {
    let fixture = Fixture::new();
    let mut port = fixture.adapter();
    let alias = fixture.temp_path().join("app-alias");
    std::os::unix::fs::symlink(&fixture.app, &alias).unwrap();
    let op = OperationId::try_from_u64(1).unwrap();
    let directory = fixture.effect_dir(op, 0);
    fs::create_dir_all(&directory).unwrap();

    port.proof_record_events();
    port.proof_sync_chain_with_library_spelling(&directory, &alias)
        .unwrap();

    let app = fs::canonicalize(&fixture.app).unwrap();
    let above = app.parent().unwrap();
    let synced: Vec<&Path> = port
        .proof_events()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            ProofEvent::Synced(path) => Some(path.as_path()),
            ProofEvent::PublicEffect => None,
        })
        .collect();
    assert_eq!(synced.last().copied(), Some(app.as_path()));
    assert!(
        !synced.iter().any(|path| *path == above),
        "the chain continued above the app root into {}",
        above.display()
    );
    assert!(!synced.iter().any(|path| *path == Path::new("/")));
}

#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn sync_chain_refuses_a_directory_outside_the_resolved_root() {
    let fixture = Fixture::new();
    let mut port = fixture.adapter();
    let outside = fixture.temp_path().join("outside-root");
    fs::create_dir_all(&outside).unwrap();

    port.proof_record_events();
    assert_eq!(
        port.proof_sync_chain_with_library_spelling(&outside, &fixture.app),
        Err(RootError::OutsideBoundary)
    );
    assert!(
        port.proof_events().unwrap().is_empty(),
        "a directory outside the app root was synced"
    );
}

#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn profile_revision_refuses_a_catalog_replaced_by_a_link() {
    let fixture = Fixture::new();
    let journal = fixture.journal();
    drop(journal);
    let mut port = fixture.adapter();
    let catalog = fixture.app.join("catalog.sqlite3");
    fs::rename(&catalog, fixture.app.join("catalog.original")).unwrap();
    std::os::unix::fs::symlink("/dev/null", &catalog).unwrap();

    assert_eq!(
        port.profile_revision(ProfileId::new(1)),
        Err(DeployError::Io)
    );
}

#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn restore_time_external_edit_conflicts_and_preserves_external_bytes() {
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
    let status =
        marduk_core::deployment::commit_deployment(&mut journal, &mut port, plan.id, || false)
            .unwrap();
    let deploy = status.id;
    let generation = status.generation.unwrap();
    let record = journal.generation(generation).unwrap();
    let winner_row = record
        .steps
        .iter()
        .find(|row| row.prepared.spec.target.target.relative.as_str() == WINNER)
        .expect("winner deploy row");
    let restore = journal
        .claim(Claim::Restore {
            installation: fixture.installation.clone(),
            data: fixture.data.clone(),
            generation,
        })
        .unwrap();
    let mut steps = Vec::new();
    for row in record.steps.iter().rev() {
        steps.push(
            port.prepare(
                restore,
                row.prepared.ordinal,
                StepDirection::Forward,
                &restore_spec(&row.prepared),
            )
            .unwrap(),
        );
    }
    journal.seal(restore, steps.clone()).unwrap();
    journal
        .set_phase(
            restore,
            marduk_core::deployment::Phase::Prepared,
            marduk_core::deployment::Phase::Restoring,
        )
        .unwrap();
    let step = steps
        .iter()
        .find(|step| step.spec.target.target.relative.as_str() == WINNER)
        .expect("winner restore step");
    journal
        .record_step(
            restore,
            step.ordinal,
            StepPhase::Ready,
            StepPhase::Attempting,
            None,
        )
        .unwrap();

    fs::write(fixture.target(WINNER), EDIT).unwrap();
    let expected = match &step.spec.pre {
        PreState::Regular(fingerprint) => fingerprint,
        PreState::Absent => panic!("restore pre must be regular"),
    };
    let (stage, _) = step.staged.as_ref().unwrap();
    let outcome =
        port.replace_existing_retain_displaced(&step.spec.target, expected, *stage, step.retain);
    assert!(matches!(outcome, EffectOutcome::Conflict(_)), "{outcome:?}");
    assert_eq!(fs::read(fixture.target(WINNER)).unwrap(), EDIT);
    let original = fixture.effect_dir(deploy, winner_row.prepared.ordinal);
    assert_eq!(fs::read(original.join("original")).unwrap(), BASE);
    let forward = fixture.effect_dir(restore, step.ordinal);
    assert_eq!(fs::read(forward.join("step")).unwrap(), POST);
    assert!(!forward.join("displaced").exists());
}

#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn unqualified_app_root_is_refused_before_any_write() {
    let fixture = Fixture::new();
    let unqualified = Path::new("/sys/kernel");
    assert!(!marduk_platform::effects::volume_qualified(unqualified));
    let error = NativeEffects::open(unqualified, &fixture.game, LAYOUT).unwrap_err();
    assert_eq!(error, DeployError::UnsupportedTarget);
    assert!(!fixture.app.join("effects").exists());
}
