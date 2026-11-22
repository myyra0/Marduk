//! Cross-client workflow witnesses for the shared workspace service.

use std::fs;
use std::path::{Path, PathBuf};

use marduk_workspace::{
    Code, Envelope, Member, Op, Request, Snapshot, WireError, Workspace, WorkspaceStatus,
};
use serde_json::{Value, json};
use tempfile::TempDir;

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
        fs::create_dir_all(game.join("Data")).unwrap();
        fs::write(game.join("SkyrimSE.exe"), b"").unwrap();
        let workspace = Workspace::open(&app).unwrap();
        Self {
            temp,
            app,
            game,
            workspace,
        }
    }

    fn dispatch(&mut self, request: Request) -> Envelope<Value> {
        self.workspace.dispatch(request)
    }

    fn send(&mut self, request: Value) -> Envelope<Value> {
        let request: Request = serde_json::from_value(request).unwrap();
        self.dispatch(request)
    }

    fn setup(&mut self) -> Value {
        let request = json!({
            "op": "setup",
            "args": { "gameDir": self.game.to_str().unwrap() },
        });
        expect_ok(self.send(request))
    }

    fn status(&mut self) -> Value {
        expect_ok(self.send(json!({ "op": "status", "args": {} })))
    }

    fn order(&mut self, at_revision: &str, member_ids: &[&str]) -> Envelope<Value> {
        self.send(json!({
            "op": "order",
            "args": { "atRevision": at_revision, "memberIds": member_ids },
        }))
    }

    fn revision_files(&self) -> usize {
        fs::read_dir(self.app.join("revisions")).unwrap().count()
    }
}

fn expect_ok(envelope: Envelope<Value>) -> Value {
    match envelope {
        Envelope::Ok { result, .. } => result,
        Envelope::Error { error, .. } => panic!("unexpected error: {error:?}"),
    }
}

fn expect_error(envelope: Envelope<Value>) -> Value {
    match envelope {
        Envelope::Ok { result, .. } => panic!("unexpected success: {result}"),
        Envelope::Error { error, .. } => serde_json::to_value(error).unwrap(),
    }
}

fn code(error: &Value) -> &str {
    error["code"].as_str().unwrap()
}

fn scratch_file(temp: &Path, name: &str) -> PathBuf {
    let path = temp.join(name);
    fs::write(&path, b"not a zip").unwrap();
    path
}

#[test]
fn setup_status_and_restart_share_the_authoritative_state() {
    let mut fixture = Fixture::new();
    let status = fixture.setup();

    assert_eq!(
        status["installation"]["label"],
        json!("Skyrim Special Edition")
    );
    assert!(status["installation"]["id"].is_string());
    assert_eq!(status["profile"]["revision"], json!("0"));
    assert_eq!(status["profile"]["id"], json!("1"));
    assert_eq!(status["profile"]["members"], json!([]));
    assert_eq!(status["availability"]["target"]["available"], json!(true));
    assert_eq!(status["availability"]["import"]["available"], json!(false));
    assert_eq!(
        status["availability"]["import"]["blocker"]["code"],
        json!("StageUnavailable")
    );
    assert_eq!(status["availability"]["deploy"]["available"], json!(false));
    assert_eq!(
        status["availability"]["deploy"]["blocker"]["code"],
        json!("UnsupportedTarget")
    );
    assert_eq!(
        status["availability"]["deploy"]["blocker"]["detail"]["reason"],
        json!("Configuration")
    );
    assert_eq!(status["pending"], Value::Null);
    assert_eq!(status["activeGenerationId"], Value::Null);

    let status = fixture.status();
    assert_eq!(
        status["installation"]["label"],
        json!("Skyrim Special Edition")
    );
    assert_eq!(status["availability"]["target"]["available"], json!(true));

    // A new composition reloads the persisted pair; it never adopts state from
    // the previous process.
    let mut reopened = Workspace::open(&fixture.app).unwrap();
    let status = expect_ok(reopened.dispatch(Request::new(Op::Status, json!({}))));
    assert_eq!(
        status["installation"]["label"],
        json!("Skyrim Special Edition")
    );
    assert_eq!(status["profile"]["revision"], json!("0"));
}

#[test]
fn empty_order_increments_the_profile_revision_once() {
    let mut fixture = Fixture::new();
    fixture.setup();

    let snapshot = expect_ok(fixture.order("0", &[]));
    assert_eq!(snapshot["revision"], json!("1"));
    assert_eq!(snapshot["members"], json!([]));

    let status = fixture.status();
    assert_eq!(status["profile"]["revision"], json!("1"));
    assert_eq!(fixture.revision_files(), 0);

    // Another client composition sees the committed revision, not a cached
    // sequence of its own.
    let mut other = Workspace::open(&fixture.app).unwrap();
    let status = expect_ok(other.dispatch(Request::new(Op::Status, json!({}))));
    assert_eq!(status["profile"]["revision"], json!("1"));
    assert_eq!(
        status["installation"]["label"],
        json!("Skyrim Special Edition")
    );
}

#[test]
fn stale_order_is_refused_without_a_second_increment() {
    let mut fixture = Fixture::new();
    fixture.setup();
    assert_eq!(expect_ok(fixture.order("0", &[]))["revision"], json!("1"));

    let error = expect_error(fixture.order("0", &[]));
    assert_eq!(code(&error), "StalePlan");
    assert_eq!(fixture.status()["profile"]["revision"], json!("1"));
}

#[test]
fn import_preview_reports_stage_unavailable_without_persisting() {
    let mut fixture = Fixture::new();
    fixture.setup();
    let zip = scratch_file(fixture.temp.path(), "import.zip");

    let error = expect_error(fixture.send(json!({
        "op": "import_preview",
        "args": {
            "zip": zip.to_str().unwrap(),
            "name": "Example",
            "atRevision": "0",
        },
    })));
    assert_eq!(code(&error), "StageUnavailable");
    assert_eq!(fixture.status()["profile"]["revision"], json!("0"));
    assert_eq!(fixture.status()["profile"]["members"], json!([]));
    assert_eq!(fixture.revision_files(), 0);

    let error = expect_error(fixture.send(json!({
        "op": "import_apply",
        "args": { "importId": "1" },
    })));
    assert_eq!(code(&error), "StageUnavailable");
}

#[test]
fn deploy_preview_apply_and_restore_are_typed_unavailable() {
    let mut fixture = Fixture::new();
    fixture.setup();

    for envelope in [
        fixture.send(json!({
            "op": "deploy_preview",
            "args": { "atRevision": "0" },
        })),
        fixture.send(json!({
            "op": "deploy_apply",
            "args": { "planId": "1" },
        })),
        fixture.send(json!({ "op": "restore", "args": {} })),
    ] {
        let error = expect_error(envelope);
        assert_eq!(code(&error), "UnsupportedTarget");
        assert_eq!(error["detail"]["reason"], json!("Configuration"));
    }

    // The same typed unavailability is visible before the user tries.
    let status = fixture.status();
    assert_eq!(
        status["availability"]["restore"]["blocker"]["code"],
        json!("UnsupportedTarget")
    );
    assert_eq!(
        status["availability"]["restore"]["blocker"]["detail"]["reason"],
        json!("Configuration")
    );
}

#[test]
fn operation_read_reports_unknown_without_writes() {
    let mut fixture = Fixture::new();
    fixture.setup();
    let error = expect_error(fixture.send(json!({
        "op": "operation",
        "args": { "operationId": "1" },
    })));
    assert_eq!(code(&error), "UnknownOperation");
    assert_eq!(fixture.status()["pending"], Value::Null);
}

#[test]
fn invalid_requests_are_refused_without_writes() {
    let mut fixture = Fixture::new();
    fixture.setup();

    let error = expect_error(fixture.send(json!({
        "op": "order",
        "args": { "atRevision": "0", "memberIds": [], "extra": true },
    })));
    assert_eq!(code(&error), "InvalidRequest");

    for revision in ["", "01", "-1", "1.0", "9223372036854775808"] {
        let error = expect_error(fixture.order(revision, &[]));
        assert_eq!(code(&error), "InvalidRequest", "revision {revision:?}");
    }

    let error = expect_error(fixture.order("0", &["0"]));
    assert_eq!(code(&error), "InvalidRequest");

    let error = expect_error(fixture.send(json!({ "op": "status", "args": { "path": "/" } })));
    assert_eq!(code(&error), "InvalidRequest");

    assert_eq!(fixture.status()["profile"]["revision"], json!("0"));
    assert_eq!(fixture.revision_files(), 0);
}

#[test]
fn no_generic_shell_or_file_operation_is_part_of_the_wire() {
    assert!(
        serde_json::from_value::<Request>(json!({ "op": "shell", "args": {} })).is_err(),
        "a shell operation must not decode"
    );
    assert!(
        serde_json::from_value::<Request>(json!({
            "op": "write_file",
            "args": { "path": "/tmp/x", "bytes": [] },
        }))
        .is_err(),
        "a generic file write must not decode"
    );
}

#[test]
fn ids_and_revisions_serialize_as_exact_decimal_strings() {
    let large_id = "9007199254740993"; // 2^53 + 1
    let large_revision = "9223372036854775807"; // i64::MAX
    let value = serde_json::to_value(Snapshot {
        id: large_id.to_owned(),
        revision: large_revision.to_owned(),
        members: vec![Member {
            id: large_id.to_owned(),
            installed_id: large_revision.to_owned(),
            enabled: true,
            display_name: "Big".to_owned(),
        }],
    })
    .unwrap();

    assert_eq!(value["id"], json!(large_id));
    assert_eq!(value["revision"], json!(large_revision));
    assert_eq!(value["members"][0]["id"], json!(large_id));
    assert_eq!(value["members"][0]["installedId"], json!(large_revision));
    assert!(value["id"].is_string());
    assert!(value["revision"].is_string());

    let status = serde_json::to_value(WorkspaceStatus {
        installation: None,
        profile: Snapshot {
            id: "1".to_owned(),
            revision: large_revision.to_owned(),
            members: Vec::new(),
        },
        availability: marduk_workspace::AvailabilitySet {
            target: marduk_workspace::Availability {
                available: true,
                blocker: None,
            },
            import: marduk_workspace::Availability {
                available: false,
                blocker: Some(WireError::new(Code::StageUnavailable)),
            },
            deploy: marduk_workspace::Availability {
                available: false,
                blocker: Some(WireError::new(Code::UnsupportedTarget)),
            },
            restore: marduk_workspace::Availability {
                available: false,
                blocker: Some(WireError::new(Code::UnsupportedTarget)),
            },
        },
        pending: None,
        active_generation_id: Some(large_id.to_owned()),
    })
    .unwrap();

    assert_eq!(status["profile"]["revision"], json!(large_revision));
    assert_eq!(status["activeGenerationId"], json!(large_id));
    assert!(status["profile"]["revision"].is_string());
}
