//! Cross-catalog installation lease witnesses.
//!
//! Two `Workspace` values with different app-data roots drive the same physical
//! installation. The lease lives outside both roots, so the second catalog can
//! neither deploy over the first nor restore it. Run with `--ignored` and a
//! `TMPDIR` on a qualified volume (tmpfs or Btrfs on Linux).

#[allow(dead_code)]
#[path = "../../archive/tests/fixtures/mod.rs"]
mod zip_fixture;

use std::fs;
use std::path::{Path, PathBuf};

use marduk_core::deployment::PlanId;
use marduk_core::ids::ProfileRevision;
use marduk_platform::lease::{InstallationLease, LeaseRecord};
use marduk_workspace::{Code, Envelope, Op, Request, RestoreResult, WireError, Workspace};
use serde_json::{Value, json};
use tempfile::TempDir;
use zip_fixture::{ClassicZip, Record};

fn setup(workspace: &mut Workspace, game: &Path) {
    expect_ok(workspace.dispatch(Request::new(
        Op::Setup,
        json!({ "gameDir": game.to_str().unwrap() }),
    )));
}

/// Import and order one pair, then return the reviewed plan.
fn review_plan(workspace: &mut Workspace, zip: &Path) -> PlanId {
    let status = expect_ok(workspace.dispatch(Request::new(Op::Status, json!({}))));
    let at_revision = status["profile"]["revision"].as_str().unwrap().to_owned();
    let prepared = expect_ok(workspace.dispatch(Request::new(
        Op::ImportPreview,
        json!({
            "zip": zip.to_str().unwrap(),
            "name": "Lease pair",
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
    let revision = ordered["revision"].as_str().unwrap().parse().unwrap();
    let preview = workspace
        .proof_preview_deployment(ProfileRevision::new(revision))
        .unwrap();
    PlanId::try_from_u64(preview.id.as_ref().unwrap().parse().unwrap()).unwrap()
}

const BASE: &[u8] = b"base\n";
const POST: &[u8] = b"b\n";

fn expect_ok(envelope: Envelope<Value>) -> Value {
    match envelope {
        Envelope::Ok { result } => result,
        Envelope::Error { error } => panic!("unexpected error: {error:?}"),
    }
}

fn expect_refusal(envelope: Envelope<Value>) -> WireError {
    match envelope {
        Envelope::Error { error } => error,
        Envelope::Ok { result } => panic!("unexpected success: {result}"),
    }
}

fn state_root(temp: &Path) -> PathBuf {
    temp.join("marduk").join("leases")
}

fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

struct Fixture {
    temp: TempDir,
    game: PathBuf,
    app_a: PathBuf,
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
        let workspace_a = Workspace::open_at(&app_a, &state_root(temp.path())).unwrap();
        let workspace_b = Workspace::open_at(&app_b, &state_root(temp.path())).unwrap();
        Self {
            temp,
            game,
            app_a,
            zip,
            workspace_a,
            workspace_b,
        }
    }

    fn lease_path(&self) -> PathBuf {
        let identity = marduk_platform::effects::directory_identity(&self.game).unwrap();
        state_root(self.temp.path()).join(format!("{}.lease", hex(&identity.installation_key())))
    }

    fn lease(&self) -> InstallationLease {
        let identity = marduk_platform::effects::directory_identity(&self.game).unwrap();
        InstallationLease::open(
            &state_root(self.temp.path()),
            &hex(&identity.installation_key()),
        )
        .unwrap()
    }

    fn record(&self, reference: &str) -> LeaseRecord {
        LeaseRecord {
            app_data: self.app_a.clone(),
            reference: Some(reference.to_owned()),
        }
    }

    fn deployed(&self) -> Vec<u8> {
        fs::read(self.game.join("Data/textures/winner.dds")).unwrap()
    }
}

#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn a_foreign_catalog_is_refused_until_the_owner_restores() {
    let mut fixture = Fixture::new();
    setup(&mut fixture.workspace_a, &fixture.game);
    let plan = review_plan(&mut fixture.workspace_a, &fixture.zip);
    let applied = fixture.workspace_a.proof_apply_deployment(plan).unwrap();
    assert_eq!(applied.phase, "completed");
    assert_eq!(fixture.deployed(), POST);

    setup(&mut fixture.workspace_b, &fixture.game);
    let foreign_plan = review_plan(&mut fixture.workspace_b, &fixture.zip);
    let refused = fixture
        .workspace_b
        .proof_apply_deployment(foreign_plan)
        .expect_err("a foreign claim refuses the deploy");
    assert_eq!(refused.code, Code::ActiveDeployment);
    let message = refused.message.expect("the refusal names the owner");
    assert!(
        message.contains(&fixture.app_a.to_string_lossy().into_owned()),
        "the refusal names the owning catalog root: {message}"
    );
    assert_eq!(fixture.deployed(), POST, "a refused deploy writes nothing");

    // Status exposes the same owner instead of a bare Busy.
    let status = expect_ok(
        fixture
            .workspace_b
            .dispatch(Request::new(Op::Status, json!({}))),
    );
    assert_eq!(
        status["availability"]["deploy"]["blocker"]["code"],
        json!("ActiveDeployment")
    );
    let status_message = status["availability"]["deploy"]["blocker"]["message"]
        .as_str()
        .expect("the status blocker names the owner");
    assert!(
        status_message.contains(&fixture.app_a.to_string_lossy().into_owned()),
        "the status blocker names the owning catalog root: {status_message}"
    );

    // Only the owner may restore; afterwards the record is gone and the other
    // catalog may deploy.
    let restored = fixture.workspace_a.proof_restore().unwrap();
    let RestoreResult::Operation { operation } = restored else {
        panic!("expected a tracked restore, got {restored:?}");
    };
    assert_eq!(operation.phase, "completed");
    assert_eq!(fixture.deployed(), BASE);
    assert!(
        !fixture.lease_path().exists() || fs::read(fixture.lease_path()).unwrap().is_empty(),
        "a completed restore clears the record"
    );

    // The plan reviewed before the restore names the deployed bytes as its
    // originals, so the second catalog reviews again and now succeeds.
    let second_plan = review_plan(&mut fixture.workspace_b, &fixture.zip);
    let applied = fixture
        .workspace_b
        .proof_apply_deployment(second_plan)
        .unwrap();
    assert_eq!(applied.phase, "completed");
    assert_eq!(fixture.deployed(), POST);
}

#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn the_record_survives_a_restart_and_a_torn_record_refuses() {
    let mut fixture = Fixture::new();
    setup(&mut fixture.workspace_a, &fixture.game);
    let plan = review_plan(&mut fixture.workspace_a, &fixture.zip);
    fixture.workspace_a.proof_apply_deployment(plan).unwrap();
    drop(fixture.workspace_a);
    // A fresh workspace is a restart of the same catalog.
    fixture.workspace_a =
        Workspace::open_at(&fixture.app_a, &state_root(fixture.temp.path())).unwrap();

    setup(&mut fixture.workspace_b, &fixture.game);
    let foreign_plan = review_plan(&mut fixture.workspace_b, &fixture.zip);
    let refused = fixture
        .workspace_b
        .proof_apply_deployment(foreign_plan)
        .expect_err("the record outlives the owning process");
    assert_eq!(refused.code, Code::ActiveDeployment);

    // The owning catalog still restores after the restart.
    let restored = fixture.workspace_a.proof_restore().unwrap();
    assert!(matches!(
        restored,
        RestoreResult::Operation { operation } if operation.phase == "completed"
    ));
    assert_eq!(fixture.deployed(), BASE);

    // Deploy again and corrupt the record: a present-but-unparseable record is
    // a typed refusal, never an absent one.
    let plan = review_plan(&mut fixture.workspace_a, &fixture.zip);
    fixture.workspace_a.proof_apply_deployment(plan).unwrap();
    let path = fixture.lease_path();
    assert!(path.exists(), "the claim is on disk");
    // The reviewed plan exists before the corruption; only the arbitration
    // record is damaged.
    let torn_plan = review_plan(&mut fixture.workspace_b, &fixture.zip);
    fs::write(&path, b"marduk-lease/1\ntorn").unwrap();

    let refused = fixture
        .workspace_b
        .proof_apply_deployment(torn_plan)
        .expect_err("a torn record refuses");
    assert_eq!(refused.code, Code::LeaseUnreadable);
    assert_eq!(
        refused.message.as_deref(),
        Some("the installation lease record is unreadable")
    );

    // Status names the unreadable record instead of claiming a clean root.
    let status = expect_refusal(
        fixture
            .workspace_b
            .dispatch(Request::new(Op::Status, json!({}))),
    );
    assert_eq!(status.code, Code::LeaseUnreadable);

    let restore = fixture
        .workspace_a
        .proof_restore()
        .expect_err("the owner also refuses an unreadable record");
    assert_eq!(restore.code, Code::LeaseUnreadable);
}

#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn a_failed_deploy_that_rolls_back_leaves_no_record() {
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

    let path = fixture.lease_path();
    assert!(
        !path.exists() || fs::read(&path).unwrap().is_empty(),
        "a fully rolled back deploy leaves no claim"
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
fn a_stale_clear_leaves_a_newer_record_intact() {
    let fixture = Fixture::new();
    let lease = fixture.lease();

    // A restore that already finished (or a replay served to a concurrent
    // call) holds only its own reference. A newer generation was written in
    // the meantime and must survive.
    lease
        .claim(&fixture.record("restore:generation:1:111-222"))
        .unwrap();
    lease.claim(&fixture.record("generation:2")).unwrap();
    assert!(
        !lease
            .clear_if(&fixture.app_a, Some("restore:generation:1:111-222"))
            .unwrap(),
        "the old restore reference no longer owns the record"
    );
    assert_eq!(
        lease.read().unwrap().unwrap().reference.as_deref(),
        Some("generation:2")
    );

    // The rollback path has the same rule for a plan reference.
    lease.claim(&fixture.record("plan:6")).unwrap();
    assert!(
        !lease.clear_if(&fixture.app_a, Some("plan:5")).unwrap(),
        "the stale plan reference no longer owns the record"
    );
    assert_eq!(
        lease.read().unwrap().unwrap().reference.as_deref(),
        Some("plan:6")
    );
}

#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn recover_keeps_the_record_while_a_generation_is_active() {
    let mut fixture = Fixture::new();
    setup(&mut fixture.workspace_a, &fixture.game);
    let plan = review_plan(&mut fixture.workspace_a, &fixture.zip);
    fixture.workspace_a.proof_apply_deployment(plan).unwrap();

    let lease = fixture.lease();
    let record = lease.read().unwrap().expect("the deploy records its claim");
    assert!(
        record
            .reference
            .as_deref()
            .is_some_and(|reference| reference.starts_with("generation:")),
        "a successful deploy names the committed generation: {record:?}"
    );

    // Nothing is pending, but an active generation is still deployed; recovery
    // must answer `None` without clearing the record.
    assert!(
        fixture
            .workspace_a
            .proof_recover_pending()
            .unwrap()
            .is_none()
    );
    assert_eq!(lease.read().unwrap(), Some(record));
}

#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn a_live_plan_claim_survives_restore_and_recover() {
    let mut fixture = Fixture::new();
    setup(&mut fixture.workspace_a, &fixture.game);
    let lease = fixture.lease();
    // A deploy between its lease claim and its journal claim leaves exactly
    // this: a `plan:` record and an idle journal.
    lease
        .claim(&LeaseRecord {
            app_data: fixture.app_a.clone(),
            reference: Some("plan:999".to_owned()),
        })
        .unwrap();

    let restored = fixture.workspace_a.proof_restore().unwrap();
    assert!(matches!(restored, RestoreResult::NoActiveDeployment { .. }));
    assert_eq!(
        lease.read().unwrap().and_then(|record| record.reference),
        Some("plan:999".to_owned()),
        "a restore with an idle journal must not clear a deploy's live claim"
    );

    assert!(
        fixture
            .workspace_a
            .proof_recover_pending()
            .unwrap()
            .is_none()
    );
    assert_eq!(
        lease.read().unwrap().and_then(|record| record.reference),
        Some("plan:999".to_owned()),
        "recovery must leave a plan claim it did not write"
    );
}

#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn a_crashed_recovery_claim_is_cleared_when_the_journal_is_idle() {
    let mut fixture = Fixture::new();
    setup(&mut fixture.workspace_a, &fixture.game);
    let lease = fixture.lease();
    lease
        .claim(&LeaseRecord {
            app_data: fixture.app_a.clone(),
            reference: Some("recovery:123-1-1".to_owned()),
        })
        .unwrap();

    assert!(
        fixture
            .workspace_a
            .proof_recover_pending()
            .unwrap()
            .is_none()
    );
    assert_eq!(
        lease.read().unwrap(),
        None,
        "an idle recovery claim this family wrote is the crash leftover and is cleared"
    );
}
