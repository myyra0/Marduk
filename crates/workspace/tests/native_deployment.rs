//! End-to-end native deployment through the workspace's synthetic proof lane.
//!
//! The installation and library are disposable trees under `TMPDIR`. Run with
//! `--ignored` and a `TMPDIR` on tmpfs or Btrfs.

#![cfg(target_os = "linux")]

#[allow(dead_code)]
#[path = "../../archive/tests/fixtures/mod.rs"]
mod zip_fixture;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use marduk_core::deployment::PlanId;
use marduk_core::ids::ProfileRevision;
use marduk_workspace::{Code, Envelope, RestoreResult, Workspace, WorkspaceStatus};
use serde_json::{Value, json};
use tempfile::TempDir;
use zip_fixture::{ClassicZip, Record};

const BASE: &[u8] = b"base\n";
const POST: &[u8] = b"b\n";
const ONLY: &[u8] = b"only-a\n";

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

fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, relative: &Path, found: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(root.join(relative)).unwrap() {
            let entry = entry.unwrap();
            let path = relative.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                walk(root, &path, found);
            } else {
                found.insert(
                    path.to_string_lossy().replace('\\', "/"),
                    fs::read(root.join(&path)).unwrap(),
                );
            }
        }
    }
    let mut found = BTreeMap::new();
    walk(root, Path::new(""), &mut found);
    found
}

struct Fixture {
    temp: TempDir,
    app: PathBuf,
    game: PathBuf,
    workspace: Workspace,
    initial: BTreeMap<String, Vec<u8>>,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        assert!(
            marduk_platform::effects::volume_qualified(temp.path()),
            "this witness needs a qualified volume; point TMPDIR at tmpfs or Btrfs"
        );
        let app = temp.path().join("app");
        let game = temp.path().join("Skyrim Special Edition");
        fs::create_dir_all(game.join("Data/textures")).unwrap();
        fs::write(game.join("SkyrimSE.exe"), b"").unwrap();
        fs::write(game.join("Data/textures/winner.dds"), BASE).unwrap();
        fs::write(game.join("Data/textures/untouched.txt"), b"keep\n").unwrap();
        let initial = tree(&game);
        let workspace = Workspace::open(&app).unwrap();
        Self {
            temp,
            app,
            game,
            workspace,
            initial,
        }
    }

    fn send(&mut self, request: Value) -> Envelope<Value> {
        self.workspace
            .dispatch(serde_json::from_value(request).unwrap())
    }

    fn setup(&mut self) -> Value {
        expect_ok(self.send(json!({
            "op": "setup",
            "args": { "gameDir": self.game.to_str().unwrap() },
        })))
    }

    fn zip(&self) -> PathBuf {
        let path = self.temp.path().join("native.zip");
        let bytes = ClassicZip::new(&[
            Record::file(b"textures/winner.dds", POST),
            Record::file(b"textures/a-only.dds", ONLY),
        ])
        .bytes;
        fs::write(&path, bytes).unwrap();
        path
    }
}

#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn qualified_native_round_trip_retains_backups_and_restores_the_original_tree() {
    let mut fixture = Fixture::new();
    let status = fixture.setup();

    // The C1 gate keeps client-visible deploy/restore unavailable even on a
    // qualified volume; the synthetic proof lane below is the adapter witness.
    assert_eq!(status["availability"]["deploy"]["available"], json!(false));
    assert_eq!(status["availability"]["restore"]["available"], json!(false));

    let zip = fixture.zip();
    let prepared = expect_ok(fixture.send(json!({
        "op": "import_preview",
        "args": {
            "zip": zip.to_str().unwrap(),
            "name": "Native pair",
            "atRevision": "0",
        },
    })));
    let imported = expect_ok(fixture.send(json!({
        "op": "import_apply",
        "args": { "importId": prepared["id"] },
    })));
    assert_eq!(imported["members"].as_array().unwrap().len(), 1);

    let ordered = expect_ok(fixture.send(json!({
        "op": "order",
        "args": {
            "atRevision": imported["revision"],
            "memberIds": [imported["members"][0]["id"].as_str().unwrap()],
        },
    })));
    let revision: u64 = ordered["revision"].as_str().unwrap().parse().unwrap();

    let preview = fixture
        .workspace
        .proof_preview_deployment(ProfileRevision::new(revision))
        .unwrap();
    assert_eq!(preview.files.len(), 2);
    let plan = PlanId::try_from_u64(preview.id.as_ref().unwrap().parse().unwrap()).unwrap();
    let operation = fixture.workspace.proof_apply_deployment(plan).unwrap();
    assert_eq!(operation.phase, "completed");
    let operation_id: u64 = operation.id.parse().unwrap();
    let generation = operation.generation_id.clone().expect("generation");

    let after = tree(&fixture.game);
    assert_eq!(after["Data/textures/winner.dds"], POST);
    assert_eq!(after["Data/textures/a-only.dds"], ONLY);
    assert_eq!(after["Data/textures/untouched.txt"], b"keep\n");

    // Each intent's original backup is the exact pre-deploy artifact; a new
    // file has no original slot.
    for (ordinal, file) in preview.files.iter().enumerate() {
        let forward = fixture
            .app
            .join("effects/ops")
            .join(operation_id.to_string())
            .join(ordinal.to_string())
            .join("forward");
        let relative = format!("Data/{}", file.target.path);
        if file.needs_backup {
            assert_eq!(
                fs::read(forward.join("original")).unwrap(),
                fixture.initial[&relative],
                "original backup for {relative}"
            );
        } else {
            assert!(!forward.join("original").exists());
        }
    }

    // Restart: the committed generation is durable and no pending work re-runs.
    drop(fixture.workspace);
    fixture.workspace = Workspace::open(&fixture.app).unwrap();
    let status: WorkspaceStatus = serde_json::from_value(expect_ok(fixture.send(json!({
        "op": "status",
        "args": {},
    }))))
    .unwrap();
    assert!(status.pending.is_none());
    assert_eq!(status.active_generation_id, Some(generation.clone()));
    assert!(fixture.workspace.proof_recover_pending().unwrap().is_none());
    assert_eq!(tree(&fixture.game)["Data/textures/winner.dds"], POST);

    let restore = fixture.workspace.proof_restore().unwrap();
    let RestoreResult::Operation { operation } = restore else {
        panic!("expected a tracked restore, got {restore:?}");
    };
    assert_eq!(operation.phase, "completed");
    assert_eq!(operation.generation_id, None);

    assert_eq!(tree(&fixture.game), fixture.initial);

    // A second restore is a typed no-op and changes nothing.
    let second = fixture.workspace.proof_restore().unwrap();
    assert!(matches!(
        second,
        RestoreResult::NoActiveDeployment { code } if code == Code::NoActiveDeployment
    ));
    assert_eq!(tree(&fixture.game), fixture.initial);
}

#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn dispatch_stays_typed_unavailable_after_native_composition() {
    let mut fixture = Fixture::new();
    fixture.setup();

    let error = expect_error(fixture.send(json!({
        "op": "deploy_preview",
        "args": { "atRevision": "0" },
    })));
    assert_eq!(error["code"], json!("UnsupportedTarget"));
    assert_eq!(error["detail"]["reason"], json!("Configuration"));

    let error = expect_error(fixture.send(json!({ "op": "restore", "args": {} })));
    assert_eq!(error["code"], json!("UnsupportedTarget"));
}
