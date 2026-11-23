//! Installation deployment-marker witnesses.
//!
//! Two `Workspace` values with different app-data roots drive the same physical
//! installation. The marker lives in the installation root, so the second
//! catalog can neither deploy over the first nor restore it. Run with
//! `--ignored` and a `TMPDIR` on a qualified volume (tmpfs or Btrfs on Linux).

#[allow(dead_code)]
#[path = "../../archive/tests/fixtures/mod.rs"]
mod zip_fixture;

use std::fs;
use std::path::{Path, PathBuf};

use marduk_core::deployment::PlanId;
use marduk_core::ids::ProfileRevision;
use marduk_core::recovery::Journal;
use marduk_games::skyrim_se::LAYOUT;
use marduk_platform::DEPLOYMENT_MARKER;
use marduk_workspace::{Code, Envelope, Op, Request, RestoreResult, WireError, Workspace};
use serde_json::{Value, json};
use tempfile::TempDir;
use zip_fixture::{ClassicZip, Record};

const BASE: &[u8] = b"base\n";
const POST: &[u8] = b"b\n";

fn expect_ok(envelope: Envelope<Value>) -> Value {
    match envelope {
        Envelope::Ok { result } => result,
        Envelope::Error { error } => panic!("unexpected error: {error:?}"),
    }
}

fn setup(workspace: &mut Workspace, game: &Path) {
    expect_ok(workspace.dispatch(Request::new(
        Op::Setup,
        json!({ "gameDir": game.to_str().unwrap() }),
    )));
}

fn arrange_pair(workspace: &mut Workspace, zip: &Path) -> u64 {
    let status = expect_ok(workspace.dispatch(Request::new(Op::Status, json!({}))));
    let at_revision = status["profile"]["revision"].as_str().unwrap().to_owned();
    let prepared = expect_ok(workspace.dispatch(Request::new(
        Op::ImportPreview,
        json!({
            "zip": zip.to_str().unwrap(),
            "name": "Marker pair",
            "atRevision": at_revision,
        }),
    )));
    let imported = expect_ok(workspace.dispatch(Request::new(
        Op::ImportApply,
        json!({ "importId": prepared["id"] }),
    )));
    let members: Vec<Value> = imported["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|member| member["id"].clone())
        .collect();
    let ordered = expect_ok(workspace.dispatch(Request::new(
        Op::Order,
        json!({
            "atRevision": imported["revision"],
            "memberIds": members,
        }),
    )));
    ordered["revision"].as_str().unwrap().parse().unwrap()
}

fn review_plan(workspace: &mut Workspace, zip: &Path) -> PlanId {
    let revision = arrange_pair(workspace, zip);
    let preview = workspace
        .proof_preview_deployment(ProfileRevision::new(revision))
        .unwrap();
    PlanId::try_from_u64(preview.id.as_ref().unwrap().parse().unwrap()).unwrap()
}

struct Fixture {
    temp: TempDir,
    game: PathBuf,
    app_a: PathBuf,
    app_b: PathBuf,
    zip: PathBuf,
    workspace_a: Workspace,
    workspace_b: Workspace,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        assert!(
            marduk_platform::effects::volume_qualified(temp.path()),
            "this witness needs a qualified volume; point TMPDIR at tmpfs or Btrfs"
        );
        let game = temp.path().join("Skyrim Special Edition");
        fs::create_dir_all(game.join("Data/textures")).unwrap();
        fs::write(game.join("SkyrimSE.exe"), b"").unwrap();
        fs::write(game.join("Data/textures/winner.dds"), BASE).unwrap();
        let zip = temp.path().join("pair.zip");
        fs::write(
            &zip,
            ClassicZip::new(&[
                Record::file(b"textures/winner.dds", POST),
                Record::file(b"textures/a-only.dds", b"only-a\n"),
            ])
            .bytes,
        )
        .unwrap();
        let app_a = temp.path().join("app-a");
        let app_b = temp.path().join("app-b");
        let workspace_a = Workspace::open(&app_a).unwrap();
        let workspace_b = Workspace::open(&app_b).unwrap();
        Self {
            temp,
            game,
            app_a,
            app_b,
            zip,
            workspace_a,
            workspace_b,
        }
    }

    fn marker(&self) -> PathBuf {
        self.game.join(DEPLOYMENT_MARKER)
    }

    fn marker_owner(&self) -> Option<String> {
        use marduk_platform::DeploymentOwner;
        let binding = marduk_platform::bind_installation(&self.game, &LAYOUT).unwrap();
        match binding.deployment_owner(&self.app_a).unwrap() {
            DeploymentOwner::Vacant => None,
            DeploymentOwner::Owned(path) | DeploymentOwner::Foreign(path) => {
                Some(path.to_string_lossy().into_owned())
            }
        }
    }

    fn claim_marker(&self, owner: &Path) {
        marduk_platform::bind_installation(&self.game, &LAYOUT)
            .unwrap()
            .claim_deployment(owner)
            .unwrap();
    }

    fn deployed(&self) -> Vec<u8> {
        fs::read(self.game.join("Data/textures/winner.dds")).unwrap()
    }
}

fn assert_owner_named(error: &WireError, owner: &Path) {
    let message = error
        .message
        .as_deref()
        .expect("the refusal names the owner");
    assert!(
        message.contains(&owner.to_string_lossy().into_owned()),
        "the refusal names the owning catalog root: {message}"
    );
}

#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn a_foreign_marker_refuses_preview_deploy_restore_and_recover_without_a_write() {
    let mut fixture = Fixture::new();
    setup(&mut fixture.workspace_a, &fixture.game);
    let plan = review_plan(&mut fixture.workspace_a, &fixture.zip);
    let applied = fixture.workspace_a.proof_apply_deployment(plan).unwrap();
    assert_eq!(applied.phase, "completed");
    assert_eq!(fixture.deployed(), POST);
    assert_eq!(
        fixture.marker_owner().as_deref(),
        Some(fixture.app_a.to_string_lossy().as_ref())
    );

    setup(&mut fixture.workspace_b, &fixture.game);

    // Preview refuses before a plan is persisted.
    let revision = arrange_pair(&mut fixture.workspace_b, &fixture.zip);
    let refused = fixture
        .workspace_b
        .proof_preview_deployment(ProfileRevision::new(revision))
        .expect_err("a foreign marker refuses the preview");
    assert_eq!(refused.code, Code::ActiveDeployment);
    assert_owner_named(&refused, &fixture.app_a);
    let mut journal =
        marduk_storage::operation_journal::OperationJournal::open(&fixture.app_b).unwrap();
    assert!(
        journal
            .plan(PlanId::try_from_u64(1).unwrap())
            .unwrap()
            .is_none(),
        "a refused preview persists no plan"
    );

    // Deploy refuses before any game write.
    let refused = fixture
        .workspace_b
        .proof_apply_deployment(PlanId::try_from_u64(1).unwrap())
        .expect_err("a foreign marker refuses the deploy");
    assert_eq!(refused.code, Code::ActiveDeployment);
    assert_owner_named(&refused, &fixture.app_a);
    assert_eq!(fixture.deployed(), POST, "a refused deploy writes nothing");

    // Restore refuses even though this catalog has no active generation.
    let refused = fixture
        .workspace_b
        .proof_restore()
        .expect_err("a foreign marker refuses the restore");
    assert_eq!(refused.code, Code::ActiveDeployment);
    assert_owner_named(&refused, &fixture.app_a);
    assert_eq!(fixture.deployed(), POST, "a refused restore writes nothing");

    // Recover refuses as well.
    let refused = fixture
        .workspace_b
        .proof_recover_pending()
        .expect_err("a foreign marker refuses recovery");
    assert_eq!(refused.code, Code::ActiveDeployment);
    assert_owner_named(&refused, &fixture.app_a);
    assert_eq!(fixture.deployed(), POST, "a refused recover writes nothing");
    assert_eq!(
        fixture.marker_owner().as_deref(),
        Some(fixture.app_a.to_string_lossy().as_ref()),
        "a foreign marker is never cleared"
    );

    // Status exposes the owner and the same typed blocker.
    let status = expect_ok(
        fixture
            .workspace_b
            .dispatch(Request::new(Op::Status, json!({}))),
    );
    assert_eq!(
        status["deployment"]["owner"],
        json!(fixture.app_a.to_string_lossy())
    );
    assert_eq!(
        status["availability"]["deploy"]["blocker"]["code"],
        json!("ActiveDeployment")
    );
}

#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn a_completed_restore_removes_the_marker_and_the_other_catalog_then_deploys() {
    let mut fixture = Fixture::new();
    setup(&mut fixture.workspace_a, &fixture.game);
    let plan = review_plan(&mut fixture.workspace_a, &fixture.zip);
    let applied = fixture.workspace_a.proof_apply_deployment(plan).unwrap();
    assert_eq!(applied.phase, "completed");
    assert_eq!(fixture.deployed(), POST);

    setup(&mut fixture.workspace_b, &fixture.game);
    let refused = fixture
        .workspace_b
        .proof_apply_deployment(PlanId::try_from_u64(1).unwrap())
        .expect_err("the first catalog owns the installation");
    assert_eq!(refused.code, Code::ActiveDeployment);

    let restored = fixture.workspace_a.proof_restore().unwrap();
    let RestoreResult::Operation { operation } = restored else {
        panic!("expected a tracked restore, got {restored:?}");
    };
    assert_eq!(operation.phase, "completed");
    assert_eq!(fixture.deployed(), BASE);
    assert!(
        !fixture.marker().exists(),
        "a completed restore clears the marker"
    );

    // The second catalog reviews again and now succeeds.
    let second_plan = review_plan(&mut fixture.workspace_b, &fixture.zip);
    let applied = fixture
        .workspace_b
        .proof_apply_deployment(second_plan)
        .unwrap();
    assert_eq!(applied.phase, "completed");
    assert_eq!(fixture.deployed(), POST);
    assert_eq!(
        fixture.marker_owner().as_deref(),
        Some(fixture.app_b.to_string_lossy().as_ref()),
        "the new owner replaced the cleared marker"
    );
}

#[test]
#[ignore = "requires a qualified effect volume"]
fn failed_marker_clear_after_restore_blocks_until_cleanup_can_retry() {
    let mut fixture = Fixture::new();
    setup(&mut fixture.workspace_a, &fixture.game);
    let plan = review_plan(&mut fixture.workspace_a, &fixture.zip);
    fixture.workspace_a.proof_apply_deployment(plan).unwrap();

    #[cfg(unix)]
    let unblock = {
        use std::os::unix::fs::PermissionsExt;
        let previous = fs::metadata(&fixture.game).unwrap().permissions();
        fs::set_permissions(&fixture.game, fs::Permissions::from_mode(0o555)).unwrap();
        let game = fixture.game.clone();
        move || fs::set_permissions(game, previous).unwrap()
    };
    #[cfg(windows)]
    let unblock = {
        use std::os::windows::fs::OpenOptionsExt;
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(3) // FILE_SHARE_READ | FILE_SHARE_WRITE
            .open(fixture.marker())
            .unwrap();
        move || drop(held)
    };
    let mut progress = Vec::new();
    let Envelope::Error { error } = fixture
        .workspace_a
        .dispatch_streaming(Request::new(Op::Restore, json!({})), |event| {
            progress.push(event.phase)
        })
    else {
        unblock();
        panic!("a completed restore with a blocked marker must report an error");
    };
    assert_eq!(error.code, Code::Io);
    assert!(!progress.iter().any(|phase| phase == "completed"));
    assert_eq!(fixture.deployed(), BASE);
    assert!(fixture.marker().exists());
    assert_eq!(
        fixture
            .workspace_a
            .proof_recover_pending()
            .unwrap_err()
            .code,
        Code::Io
    );

    unblock();
    let edited = fixture.game.join("Data/textures/winner.dds");
    fs::write(&edited, b"after restore\n").unwrap();
    assert!(
        fixture
            .workspace_a
            .proof_recover_pending()
            .unwrap()
            .is_none()
    );
    assert_eq!(fs::read(edited).unwrap(), b"after restore\n");
    assert!(!fixture.marker().exists());
}

#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn a_successful_deploy_keeps_the_marker() {
    let mut fixture = Fixture::new();
    setup(&mut fixture.workspace_a, &fixture.game);
    let plan = review_plan(&mut fixture.workspace_a, &fixture.zip);
    let applied = fixture.workspace_a.proof_apply_deployment(plan).unwrap();
    assert_eq!(applied.phase, "completed");
    assert_eq!(
        fixture.marker_owner().as_deref(),
        Some(fixture.app_a.to_string_lossy().as_ref()),
        "a successful deploy keeps the marker"
    );

    // An active generation means recovery answers `None` without clearing it.
    assert!(
        fixture
            .workspace_a
            .proof_recover_pending()
            .unwrap()
            .is_none()
    );
    assert_eq!(
        fixture.marker_owner().as_deref(),
        Some(fixture.app_a.to_string_lossy().as_ref())
    );
}

#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn an_owned_marker_with_an_idle_journal_is_cleared_by_recover() {
    let mut fixture = Fixture::new();
    setup(&mut fixture.workspace_a, &fixture.game);
    // The crash window: the marker was claimed, then the process exited before
    // recording any operation.
    fixture.claim_marker(&fixture.app_a.clone());

    assert!(
        fixture
            .workspace_a
            .proof_recover_pending()
            .unwrap()
            .is_none()
    );
    assert!(
        !fixture.marker().exists(),
        "an owned marker with an idle journal is cleared"
    );
}

#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn a_fully_rolled_back_deploy_removes_the_marker() {
    let mut fixture = Fixture::new();
    setup(&mut fixture.workspace_a, &fixture.game);
    let plan = review_plan(&mut fixture.workspace_a, &fixture.zip);

    // Change the reviewed original before the commit: the whole plan is
    // refused before its first effect, so it rolls back with nothing pending.
    fs::write(fixture.game.join("Data/textures/winner.dds"), b"external\n").unwrap();
    let refused = fixture
        .workspace_a
        .proof_apply_deployment(plan)
        .expect_err("a changed original refuses the plan");
    assert_eq!(refused.code, Code::RecoveryConflict);
    assert!(
        !fixture.marker().exists(),
        "a fully rolled back deploy removes the marker"
    );

    // A second catalog is not blocked by the abandoned claim.
    fs::write(fixture.game.join("Data/textures/winner.dds"), BASE).unwrap();
    setup(&mut fixture.workspace_b, &fixture.game);
    let plan = review_plan(&mut fixture.workspace_b, &fixture.zip);
    let applied = fixture.workspace_b.proof_apply_deployment(plan).unwrap();
    assert_eq!(applied.phase, "completed");
}

#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn recovery_after_moving_the_installation_is_target_unavailable() {
    let mut fixture = Fixture::new();
    setup(&mut fixture.workspace_a, &fixture.game);
    let moved = fixture.temp.path().join("Skyrim Special Edition-moved");
    fs::rename(&fixture.game, &moved).unwrap();

    let refused = fixture
        .workspace_a
        .proof_recover_pending()
        .expect_err("a moved installation is a lost target");
    assert_eq!(refused.code, Code::TargetUnavailable);
}
