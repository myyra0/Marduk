//! The client-visible crash reconciliation route.
//!
//! These witnesses drive `Op::Recover` through the shared workspace service.
//! The successful continuation needs a qualified effect volume; an unqualified
//! run asserts the typed refusal instead of skipping.

#![cfg(target_os = "linux")]

use std::fs;
use std::path::{Path, PathBuf};

use marduk_core::ids::{ProfileId, ProfileRevision};
use marduk_core::recovery::{Claim, Journal};
use marduk_storage::operation_journal::OperationJournal;
use marduk_workspace::{Envelope, Op, Request, Workspace};
use serde_json::{Value, json};
use tempfile::TempDir;

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
}

#[test]
fn clean_root_answers_none_without_opening_a_native_port() {
    let mut fixture = Fixture::new();
    fixture.setup();
    // An unqualified app root would fail a port open; `None` proves the route
    // answered from the journal before any native work.
    let _real = fixture.make_root_unqualified();

    assert_eq!(expect_ok(fixture.recover()), Value::Null);
    assert_eq!(fixture.pending_in(&_real), None);
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
