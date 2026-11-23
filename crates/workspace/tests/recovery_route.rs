//! The client-visible crash reconciliation route.
//!
//! These witnesses drive `Op::Recover` through the shared workspace service.
//! The successful continuation needs a qualified effect volume; an unqualified
//! run asserts the typed refusal instead of skipping.

#![cfg(target_os = "linux")]

#[allow(dead_code)]
#[path = "../../archive/tests/fixtures/mod.rs"]
mod zip_fixture;

use std::fs;
use std::path::{Path, PathBuf};

use marduk_core::deployment::{DeploymentInputs, Phase, PlanId, PreState};
use marduk_core::ids::{ProfileId, ProfileRevision};
use marduk_core::recovery::{
    Claim, CopyEffects, Journal, PreImage, StepDirection, StepOutput, StepPhase, StepSpec,
};
use marduk_games::skyrim_se;
use marduk_storage::native_effects::{DEPLOYMENT_MARKER, NativeEffects};
use marduk_storage::operation_journal::OperationJournal;
use marduk_workspace::{Envelope, Op, Request, Workspace};
use serde_json::{Value, json};
use tempfile::TempDir;
use zip_fixture::{ClassicZip, Record};

fn expect_ok(envelope: Envelope<Value>) -> Value {
    match envelope {
        Envelope::Ok { result } => result,
        Envelope::Error { error } => panic!("unexpected error: {error:?}"),
    }
}

fn expect_error(envelope: Envelope<Value>) -> Value {
    match envelope {
        Envelope::Ok { result } => panic!("unexpected success: {result}"),
        Envelope::Error { error } => serde_json::to_value(error).unwrap(),
    }
}

struct Fixture {
    temp: TempDir,
    app: PathBuf,
    game: PathBuf,
    workspace: Workspace,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("app");
        let game = temp.path().join("Skyrim Special Edition");
        fs::create_dir_all(game.join("Data/textures")).unwrap();
        fs::write(game.join("SkyrimSE.exe"), b"").unwrap();
        fs::write(game.join("Data/textures/winner.dds"), b"base\n").unwrap();
        let workspace = Workspace::open(&app).unwrap();
        Self {
            temp,
            app,
            game,
            workspace,
        }
    }

    fn setup(&mut self) {
        expect_ok(self.workspace.dispatch(Request::new(
            Op::Setup,
            json!({ "gameDir": self.game.to_str().unwrap() }),
        )));
    }

    fn recover(&mut self) -> Envelope<Value> {
        self.workspace
            .dispatch(Request::new(Op::Recover, json!({})))
    }

    /// Record a zero-step pending Deploy in the journal. A preparation crash
    /// leaves exactly this state: pending work with no executed step.
    fn pending_zero_step_deploy(&self) -> u64 {
        let mut journal = OperationJournal::open(&self.app).unwrap();
        let selected = journal
            .selected_installation()
            .unwrap()
            .expect("setup binds the installation");
        let plan = journal
            .save_plan(
                ProfileId::new(1),
                ProfileRevision::new(0),
                selected.installation,
                selected.data,
                Vec::new(),
            )
            .unwrap();
        journal.claim(Claim::Deploy(plan)).unwrap().get()
    }

    fn pending_in(&self, root: &Path) -> Option<u64> {
        let mut journal = OperationJournal::open(root).unwrap();
        let selected = journal.selected_installation().unwrap().unwrap();
        journal
            .root_state(&selected.data)
            .unwrap()
            .pending
            .map(|id| id.get())
    }

    /// Replace the app-data root with a link the effect probe refuses. Open
    /// handles keep working; only a new native port asks for the volume class.
    fn make_root_unqualified(&self) -> PathBuf {
        let real = self.temp.path().join("app-real");
        fs::rename(&self.app, &real).unwrap();
        std::os::unix::fs::symlink("/sys/kernel", &self.app).unwrap();
        real
    }

    fn game_bytes(&self) -> Vec<u8> {
        fs::read(self.game.join("Data/textures/winner.dds")).unwrap()
    }

    fn marker(&self) -> PathBuf {
        self.game.join(DEPLOYMENT_MARKER)
    }

    fn claim_marker(&self, owner: &Path) {
        fs::write(self.marker(), owner.to_string_lossy().as_bytes()).unwrap();
    }
}

#[test]
fn clean_root_on_an_unqualified_volume_answers_none() {
    let mut fixture = Fixture::new();
    fixture.setup();
    // Recovery creates no work: with nothing pending, an unqualified root
    // answers `null` from the journal instead of pretending to need a port.
    let real = fixture.make_root_unqualified();

    let result = expect_ok(fixture.recover());
    assert_eq!(result, Value::Null);
    assert_eq!(fixture.pending_in(&real), None);
}

#[test]
fn moved_installation_is_target_unavailable() {
    let mut fixture = Fixture::new();
    fixture.setup();
    fs::rename(
        &fixture.game,
        fixture.temp.path().join("Skyrim Special Edition-moved"),
    )
    .unwrap();

    let error = expect_error(fixture.recover());
    assert_eq!(error["code"], json!("TargetUnavailable"));
}

#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs: reading the marker needs a native adapter"]
fn nothing_pending_but_a_foreign_marker_refuses_and_names_the_owner() {
    let mut fixture = Fixture::new();
    fixture.setup();
    let owner = fixture.temp.path().join("other-catalog");
    fixture.claim_marker(&owner);
    let before = fixture.game_bytes();

    let error = expect_error(fixture.recover());
    assert_eq!(error["code"], json!("ActiveDeployment"));
    let message = error["message"]
        .as_str()
        .expect("the refusal names the owner");
    assert!(
        message.contains(&owner.to_string_lossy().into_owned()),
        "the refusal names the owning catalog root: {message}"
    );
    assert_eq!(
        fs::read(fixture.marker()).unwrap(),
        owner.to_string_lossy().as_bytes(),
        "a foreign marker is never cleared"
    );
    assert_eq!(
        fixture.game_bytes(),
        before,
        "a refused recover writes nothing"
    );
}

#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs: reading the marker needs a native adapter"]
fn idle_owned_marker_is_cleared_when_nothing_is_pending() {
    let mut fixture = Fixture::new();
    fixture.setup();
    fixture.claim_marker(&fixture.app.clone());

    // The crash window: the marker was claimed, then the process exited before
    // recording any operation. The journal has nothing pending, so only the
    // marker can tell recovery to unblock the installation.
    assert_eq!(expect_ok(fixture.recover()), Value::Null);
    assert!(
        !fixture.marker().exists(),
        "the claim without journal work is cleared"
    );
}

#[test]
fn unbound_root_is_a_typed_refusal() {
    let mut fixture = Fixture::new();

    let error = expect_error(fixture.recover());
    assert_eq!(error["code"], json!("TargetUnavailable"));
}

#[test]
fn unqualified_root_refuses_pending_work_without_simulating_success() {
    let mut fixture = Fixture::new();
    fixture.setup();
    let pending = fixture.pending_zero_step_deploy();
    let before = fixture.game_bytes();
    let real = fixture.make_root_unqualified();

    let error = expect_error(fixture.recover());
    assert_eq!(error["code"], json!("UnsupportedTarget"));
    assert_eq!(fixture.pending_in(&real), Some(pending));
    assert_eq!(fixture.game_bytes(), before);
}

#[test]
fn recorded_pending_work_recovers_once_then_returns_none() {
    let mut fixture = Fixture::new();
    fixture.setup();
    let pending = fixture.pending_zero_step_deploy();
    let before = fixture.game_bytes();

    if !marduk_platform::effects::volume_qualified(fixture.temp.path()) {
        // This host cannot run the native continuation; the typed refusal is
        // still the required behavior, never a simulated success.
        let error = expect_error(fixture.recover());
        assert_eq!(error["code"], json!("UnsupportedTarget"));
        assert_eq!(fixture.pending_in(&fixture.app), Some(pending));
        return;
    }

    let operation = expect_ok(fixture.recover());
    assert_eq!(operation["id"], json!(pending.to_string()));
    assert_eq!(operation["kind"], json!("deploy"));
    assert_eq!(operation["phase"], json!("cancelled"));
    assert_eq!(fixture.pending_in(&fixture.app), None);
    assert_eq!(fixture.game_bytes(), before);

    // Resolved work stays resolved: a second call performs no effect and the
    // recorded terminal outcome is unchanged.
    assert_eq!(expect_ok(fixture.recover()), Value::Null);
    assert_eq!(fixture.pending_in(&fixture.app), None);
    assert_eq!(fixture.game_bytes(), before);
    let recorded = expect_ok(fixture.workspace.dispatch(Request::new(
        Op::Operation,
        json!({ "operationId": pending.to_string() }),
    )));
    assert_eq!(recorded["phase"], json!("cancelled"));
    assert_eq!(recorded["blocker"], Value::Null);
}

#[test]
fn recover_rejects_arguments() {
    let mut fixture = Fixture::new();
    let envelope = fixture
        .workspace
        .dispatch(Request::new(Op::Recover, json!({ "operationId": "1" })));
    let error = expect_error(envelope);
    assert_eq!(error["code"], json!("InvalidRequest"));
}

/// The apply-before-ACK crash window: the single step's bytes are published,
/// but no step or final acknowledgement is recorded. Recovery must roll it back
/// exactly once; a second call answers `null` and no byte changes.
#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn one_step_apply_before_ack_recovers_once_and_preserves_bytes() {
    let mut fixture = Fixture::new();
    assert!(
        marduk_platform::effects::volume_qualified(fixture.temp.path()),
        "this witness needs a qualified volume; point TMPDIR at tmpfs or Btrfs"
    );
    fixture.setup();
    let before = fixture.game_bytes();

    let zip = fixture.temp.path().join("one-step.zip");
    fs::write(
        &zip,
        ClassicZip::new(&[Record::file(b"textures/winner.dds", b"b\n")]).bytes,
    )
    .unwrap();
    let prepared = expect_ok(fixture.workspace.dispatch(Request::new(
        Op::ImportPreview,
        json!({
            "zip": zip.to_str().unwrap(),
            "name": "One step",
            "atRevision": "0",
        }),
    )));
    let imported = expect_ok(fixture.workspace.dispatch(Request::new(
        Op::ImportApply,
        json!({ "importId": prepared["id"] }),
    )));
    let ordered = expect_ok(fixture.workspace.dispatch(Request::new(
        Op::Order,
        json!({
            "atRevision": imported["revision"],
            "memberIds": [imported["members"][0]["id"].as_str().unwrap()],
        }),
    )));
    let revision: u64 = ordered["revision"].as_str().unwrap().parse().unwrap();
    let preview = fixture
        .workspace
        .proof_preview_deployment(ProfileRevision::new(revision))
        .unwrap();
    assert_eq!(preview.files.len(), 1, "the cut witness needs one step");
    let plan_id = PlanId::try_from_u64(preview.id.as_ref().unwrap().parse().unwrap()).unwrap();

    // Drive the real commit far enough to publish the step, then stop before
    // the step ACK and the finalize. Dropping both handles is the crash.
    let pending = {
        let mut journal = OperationJournal::open(&fixture.app).unwrap();
        let selected = journal.selected_installation().unwrap().unwrap();
        let plan = journal.plan(plan_id).unwrap().unwrap();
        let op = journal.claim(Claim::Deploy(plan.clone())).unwrap();
        let mut port = NativeEffects::open(&fixture.app, &selected.locator, skyrim_se::LAYOUT)
            .expect("qualified volume opens the native port");
        port.try_root_gate().unwrap();
        port.verify_root(&plan.installation, &plan.data).unwrap();
        let intent = plan.intents[0].clone();
        let source = port
            .source(intent.source.installed, &intent.source.path)
            .unwrap();
        let spec = StepSpec {
            target: intent.target.clone(),
            original: PreImage {
                observation: intent.original.clone(),
                backup: None,
            },
            pre: intent.original.clone(),
            output: StepOutput::Source(source),
        };
        let step = port.prepare(op, 0, StepDirection::Forward, &spec).unwrap();
        journal.seal(op, vec![step.clone()]).unwrap();
        journal
            .set_phase(op, Phase::Prepared, Phase::Applying)
            .unwrap();
        journal
            .record_step(op, 0, StepPhase::Ready, StepPhase::Attempting, None)
            .unwrap();
        port.inspect_step(&step);
        match (&step.spec.pre, step.staged.as_ref()) {
            (PreState::Regular(expected), Some((stage, _))) => {
                port.replace_existing_retain_displaced(
                    &step.spec.target,
                    expected,
                    *stage,
                    step.retain,
                );
            }
            (PreState::Absent, Some((stage, _))) => {
                port.publish_new_absent(&step.spec.target, *stage);
            }
            _ => panic!("the prepared step is not executable"),
        }
        op.get()
    };

    assert_eq!(fixture.game_bytes(), b"b\n", "the cut published the bytes");

    let operation = expect_ok(fixture.recover());
    assert_eq!(operation["id"], json!(pending.to_string()));
    assert_eq!(operation["kind"], json!("deploy"));
    assert_eq!(operation["phase"], json!("rolled_back"));
    assert_eq!(operation["blocker"], Value::Null);
    assert_eq!(fixture.pending_in(&fixture.app), None);
    assert_eq!(fixture.game_bytes(), before);

    // Resolved work stays resolved: one rollback, then nothing pending.
    assert_eq!(expect_ok(fixture.recover()), Value::Null);
    assert_eq!(fixture.game_bytes(), before);
}
