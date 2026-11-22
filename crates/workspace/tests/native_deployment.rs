//! End-to-end native deployment through the workspace's synthetic proof lane.
//!
//! The installation and library are disposable trees under `TMPDIR`. Run with
//! `--ignored` and a `TMPDIR` on a qualified volume (tmpfs or Btrfs on Linux,
//! NTFS on Windows).

#[allow(dead_code)]
#[path = "../../archive/tests/fixtures/mod.rs"]
mod zip_fixture;

use std::collections::BTreeMap;
#[cfg(unix)]
use std::ffi::CString;
use std::fs;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use marduk_core::deployment::PlanId;
use marduk_core::ids::ProfileRevision;
use marduk_core::recovery::{Claim, Journal};
use marduk_storage::operation_journal::OperationJournal;
use marduk_workspace::{
    Code, Direction, Envelope, Kind, Operation, RestoreResult, Slot, Workspace, WorkspaceStatus,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use zip_fixture::{ClassicZip, Record};

const BASE: &[u8] = b"base\n";
const POST: &[u8] = b"b\n";
const MIXED: &[u8] = b"mixed\n";
const ONLY: &[u8] = b"only-a\n";
const OUTSIDE: &[u8] = b"user\n";

// The admitted original and the staged payload deliberately differ in every
// scoped metadata field Restore must reproduce, so equal fingerprints on
// identical fixtures cannot stand in for preservation.
#[cfg(unix)]
const ORIGINAL_MODE: u32 = 0o640;
#[cfg(unix)]
const STAGED_MODE: u32 = 0o600;
const ORIGINAL_MTIME: (u64, u32) = (1_500_000_000, 123_456_789);
const STAGED_MTIME: (u64, u32) = (1_600_000_000, 987_654_321);

fn stamped((seconds, nanos): (u64, u32)) -> SystemTime {
    UNIX_EPOCH + Duration::new(seconds, nanos)
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn set_mtime(path: &Path, time: SystemTime) {
    let file = fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_times(fs::FileTimes::new().set_modified(time))
        .unwrap();
}

#[cfg(unix)]
fn mode_of(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

#[cfg(unix)]
fn mtime_of(path: &Path) -> (u64, u32) {
    let modified = fs::metadata(path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap();
    (modified.as_secs(), modified.subsec_nanos())
}

#[cfg(unix)]
fn owner_of(path: &Path) -> (u32, u32) {
    let metadata = fs::metadata(path).unwrap();
    (metadata.uid(), metadata.gid())
}

#[cfg(unix)]
fn set_xattr(path: &Path, name: &std::ffi::CStr, value: &[u8]) -> std::io::Result<()> {
    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
    let result = unsafe {
        libc::setxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
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

/// Sorted child names, or none when the directory was never created.
fn entry_names(path: &Path) -> Vec<String> {
    let mut names = match fs::read_dir(path) {
        Ok(entries) => entries
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => panic!("{error}"),
    };
    names.sort();
    names
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
        fs::write(game.join("Data/outside-plan.txt"), OUTSIDE).unwrap();
        // The install target carries metadata the staged payload does not.
        let winner = game.join("Data/textures/winner.dds");
        #[cfg(unix)]
        set_mode(&winner, ORIGINAL_MODE);
        set_mtime(&winner, stamped(ORIGINAL_MTIME));
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
        self.zip_records(
            "native.zip",
            &[
                Record::file(b"textures/winner.dds", POST),
                Record::file(b"textures/a-only.dds", ONLY),
            ],
        )
    }

    fn zip_records(&self, name: &str, records: &[Record<'_>]) -> PathBuf {
        let path = self.temp.path().join(name);
        let bytes = ClassicZip::new(records).bytes;
        fs::write(&path, bytes).unwrap();
        path
    }

    /// Import one archive at the given profile revision.
    fn import(&mut self, zip: &Path, name: &str, at_revision: &str) -> Value {
        let prepared = expect_ok(self.send(json!({
            "op": "import_preview",
            "args": {
                "zip": zip.to_str().unwrap(),
                "name": name,
                "atRevision": at_revision,
            },
        })));
        expect_ok(self.send(json!({
            "op": "import_apply",
            "args": { "importId": prepared["id"] },
        })))
    }

    /// Setup, import and deploy one pair, returning the reviewed plan and the
    /// committed operation.
    fn apply_one(&mut self) -> (PlanId, Operation) {
        self.setup();
        let (revision, _) = self.import_and_order();
        let preview = self
            .workspace
            .proof_preview_deployment(ProfileRevision::new(revision))
            .unwrap();
        let plan = PlanId::try_from_u64(preview.id.as_ref().unwrap().parse().unwrap()).unwrap();
        let operation = self.workspace.proof_apply_deployment(plan).unwrap();
        (plan, operation)
    }

    /// Import the native pair and order its member, returning the new revision
    /// and the imported snapshot.
    fn import_and_order(&mut self) -> (u64, Value) {
        let zip = self.zip();
        let prepared = expect_ok(self.send(json!({
            "op": "import_preview",
            "args": {
                "zip": zip.to_str().unwrap(),
                "name": "Native pair",
                "atRevision": "0",
            },
        })));
        let imported = expect_ok(self.send(json!({
            "op": "import_apply",
            "args": { "importId": prepared["id"] },
        })));
        let ordered = expect_ok(self.send(json!({
            "op": "order",
            "args": {
                "atRevision": imported["revision"],
                "memberIds": [imported["members"][0]["id"].as_str().unwrap()],
            },
        })));
        let revision: u64 = ordered["revision"].as_str().unwrap().parse().unwrap();
        (revision, imported)
    }

    /// The single imported revision's payload for `textures/winner.dds`.
    fn staged_winner(&self) -> PathBuf {
        let revisions: Vec<PathBuf> = fs::read_dir(self.app.join("revisions"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(revisions.len(), 1, "one imported revision");
        revisions[0].join("payload/textures/winner.dds")
    }

    /// Replace the app-data root with a link the effect probe refuses. Open
    /// handles keep working; only a new port asks for the volume class.
    #[cfg(unix)]
    fn make_root_unqualified(&self) -> PathBuf {
        let real = self.temp.path().join("app-real");
        fs::rename(&self.app, &real).unwrap();
        std::os::unix::fs::symlink("/sys/kernel", &self.app).unwrap();
        real
    }
}

#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn qualified_native_round_trip_retains_backups_and_restores_the_original_tree() {
    let mut fixture = Fixture::new();
    let status = fixture.setup();

    // The record proves the composition on this volume, so the client routes
    // are available; this witness still drives the proof lane directly.
    assert_eq!(status["availability"]["deploy"]["available"], json!(true));
    assert_eq!(status["availability"]["restore"]["available"], json!(true));
    assert_eq!(status["availability"]["deploy"]["blocker"], Value::Null);

    let (revision, imported) = fixture.import_and_order();
    assert_eq!(imported["members"].as_array().unwrap().len(), 1);

    // Give the staged payload metadata that differs from the install target:
    // the copy must carry the payload's mode and time, and Restore must bring
    // back the original's. Both values are asserted exactly below.
    let staged_winner = fixture.staged_winner();
    #[cfg(unix)]
    set_mode(&staged_winner, STAGED_MODE);
    set_mtime(&staged_winner, stamped(STAGED_MTIME));
    #[cfg(unix)]
    eprintln!(
        "metadata before apply: target mode {:04o} mtime {:?} uid/gid {:?}; \
         staged mode {:04o} mtime {:?}",
        mode_of(&fixture.game.join("Data/textures/winner.dds")),
        mtime_of(&fixture.game.join("Data/textures/winner.dds")),
        owner_of(&fixture.game.join("Data/textures/winner.dds")),
        mode_of(&staged_winner),
        mtime_of(&staged_winner),
    );
    #[cfg(unix)]
    assert_ne!(ORIGINAL_MODE, STAGED_MODE);
    assert_ne!(ORIGINAL_MTIME, STAGED_MTIME);

    let preview = fixture
        .workspace
        .proof_preview_deployment(ProfileRevision::new(revision))
        .unwrap();
    assert_eq!(preview.files.len(), 2);
    let plan = PlanId::try_from_u64(preview.id.as_ref().unwrap().parse().unwrap()).unwrap();
    let operation = fixture.workspace.proof_apply_deployment(plan).unwrap();
    assert_eq!(operation.phase, "completed");
    assert!(
        operation.blocker.is_none(),
        "apply blocker: {:?}",
        operation.blocker
    );
    let operation_id: u64 = operation.id.parse().unwrap();
    let generation = operation.generation_id.clone().expect("generation");

    let deployed_winner = fixture.game.join("Data/textures/winner.dds");
    let after = tree(&fixture.game);
    assert_eq!(after["Data/textures/winner.dds"], POST);
    assert_eq!(after["Data/textures/a-only.dds"], ONLY);
    assert_eq!(after["Data/textures/untouched.txt"], b"keep\n");
    assert_eq!(after["Data/outside-plan.txt"], OUTSIDE);

    // Bytes and scoped metadata together: a metadata-only comparison cannot
    // mask a byte failure, and vice versa. The target now carries the staged
    // payload's mode, uid/gid, mtime and bytes, not the displaced original's.
    assert_eq!(fs::read(&deployed_winner).unwrap(), POST);
    #[cfg(unix)]
    {
        assert_eq!(
            mode_of(&deployed_winner),
            STAGED_MODE,
            "apply publishes the staged payload's mode"
        );
        assert_eq!(
            mtime_of(&deployed_winner),
            STAGED_MTIME,
            "apply publishes the staged payload's mtime"
        );
        assert_eq!(owner_of(&deployed_winner), owner_of(&staged_winner));
        eprintln!(
            "metadata after apply: mode {:04o} mtime {:?} uid/gid {:?}",
            mode_of(&deployed_winner),
            mtime_of(&deployed_winner),
            owner_of(&deployed_winner),
        );
    }

    // The status ref for the replaced ordinal must name the actually displaced
    // object, not only the pre-image copy. The sealed retain location is the
    // canonical `displaced` slot, or the exchange alias in `stage` on Linux.
    let replaced = preview
        .files
        .iter()
        .position(|file| file.needs_backup)
        .expect("a replaced file");
    let displaced = operation
        .retained
        .iter()
        .find(|retained| {
            retained.ordinal == replaced as u32
                && matches!(retained.direction, Direction::Forward)
                && matches!(retained.slot, Slot::Displaced)
        })
        .expect("status names the actually displaced object");
    assert_eq!(displaced.operation_id, operation.id);
    let forward = fixture
        .app
        .join("effects/ops")
        .join(operation_id.to_string())
        .join(replaced.to_string())
        .join("forward");
    let sealed = if forward.join("displaced").exists() {
        forward.join("displaced")
    } else {
        forward.join("stage")
    };
    assert_eq!(
        fs::read(&sealed).unwrap(),
        BASE,
        "the actually displaced object is retained with its original bytes"
    );
    #[cfg(unix)]
    {
        assert_eq!(
            mode_of(&sealed),
            ORIGINAL_MODE,
            "the retained displaced original keeps its mode"
        );
        assert_eq!(
            mtime_of(&sealed),
            ORIGINAL_MTIME,
            "the retained displaced original keeps its mtime"
        );
        assert_eq!(
            owner_of(&sealed),
            owner_of(&fixture.game.join("Data/textures/winner.dds"))
        );
    }

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
    assert!(fixture.workspace.proof_recover_pending().unwrap().is_none());
    assert_eq!(tree(&fixture.game)["Data/textures/winner.dds"], POST);

    // A replay of the claimed plan reports the recorded terminal status and
    // never reopens the plan as a new operation.
    let replay = fixture.workspace.proof_apply_deployment(plan).unwrap();
    assert_eq!(replay.id, operation.id);
    assert_eq!(tree(&fixture.game)["Data/textures/winner.dds"], POST);

    let restore = fixture.workspace.proof_restore().unwrap();
    let RestoreResult::Operation { operation } = restore else {
        panic!("expected a tracked restore, got {restore:?}");
    };
    assert_eq!(operation.phase, "completed");
    assert!(
        operation.blocker.is_none(),
        "restore blocker: {:?}",
        operation.blocker
    );
    assert_eq!(operation.generation_id, None);

    assert_eq!(tree(&fixture.game), fixture.initial);

    // Restore publishes the original's bytes and scoped metadata, not the
    // deployed object's: the two differ on both axes.
    let restored_winner = fixture.game.join("Data/textures/winner.dds");
    assert_eq!(fs::read(&restored_winner).unwrap(), BASE);
    #[cfg(unix)]
    {
        assert_eq!(
            mode_of(&restored_winner),
            ORIGINAL_MODE,
            "restore brings back the original mode"
        );
        assert_eq!(
            mtime_of(&restored_winner),
            ORIGINAL_MTIME,
            "restore brings back the original mtime"
        );
        eprintln!(
            "metadata after restore: mode {:04o} mtime {:?} uid/gid {:?}",
            mode_of(&restored_winner),
            mtime_of(&restored_winner),
            owner_of(&restored_winner),
        );
    }

    // Restore consumes the object it displaced into its own sealed retain
    // location; its status ref must resolve to the deployed bytes.
    let restore_id: u64 = operation.id.parse().unwrap();
    let restored = operation
        .retained
        .iter()
        .find(|retained| {
            retained.ordinal == replaced as u32
                && matches!(retained.direction, Direction::Forward)
                && matches!(retained.slot, Slot::Displaced)
        })
        .expect("restore status names its displaced object");
    assert_eq!(restored.operation_id, operation.id);
    let restore_forward = fixture
        .app
        .join("effects/ops")
        .join(restore_id.to_string())
        .join(replaced.to_string())
        .join("forward");
    let restore_sealed = if restore_forward.join("displaced").exists() {
        restore_forward.join("displaced")
    } else {
        restore_forward.join("stage")
    };
    assert_eq!(
        fs::read(&restore_sealed).unwrap(),
        POST,
        "restore retains the deployed object it consumed"
    );

    // A second restore is a typed no-op and changes nothing.
    let second = fixture.workspace.proof_restore().unwrap();
    assert!(matches!(
        second,
        RestoreResult::NoActiveDeployment { code } if code == Code::NoActiveDeployment
    ));
    assert_eq!(tree(&fixture.game), fixture.initial);
}

#[cfg(unix)]
#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn visible_xattr_on_an_original_is_refused_before_any_write() {
    let mut fixture = Fixture::new();
    fixture.setup();
    let winner = fixture.game.join("Data/textures/winner.dds");

    match set_xattr(&winner, c"user.marduk", b"x") {
        Ok(()) => {}
        Err(error)
            if matches!(
                error.raw_os_error(),
                Some(libc::EOPNOTSUPP) | Some(libc::EPERM)
            ) =>
        {
            eprintln!("not run: this scratch filesystem refuses user xattrs: {error}");
            return;
        }
        Err(error) => panic!("setting a test xattr failed: {error}"),
    }

    let (revision, _) = fixture.import_and_order();
    let error = fixture
        .workspace
        .proof_preview_deployment(ProfileRevision::new(revision))
        .expect_err("a visible xattr on the original is outside the admitted metadata set");
    assert_eq!(error.code, Code::UnsupportedTarget);
    // The composed API does not carry the platform `Metadata` reason through
    // `DeployError`; the typed code is the refusal this layer promises.
    assert_eq!(error.detail, None);

    // No game stage, operation directory, byte or metadata change: eligibility
    // refused before any write.
    assert_eq!(tree(&fixture.game), fixture.initial);
    assert_eq!(mode_of(&winner), ORIGINAL_MODE);
    assert_eq!(mtime_of(&winner), ORIGINAL_MTIME);
    let ops = fixture.app.join("effects/ops");
    assert!(
        !ops.exists() || fs::read_dir(&ops).unwrap().next().is_none(),
        "a refused preview creates no operation-owned directory"
    );

    // `trusted.*` is excluded by the contract as unobserved, and only a
    // privileged process can attach it; an ordinary run cannot exercise what an
    // unprivileged probe would do. Record the refusal of the attempt, not a pass.
    match set_xattr(&winner, c"trusted.marduk", b"x") {
        Ok(()) => eprintln!(
            "not run: this environment can attach trusted.*; the unprivileged probe case \
             was not exercised"
        ),
        Err(error) if error.raw_os_error() == Some(libc::EPERM) => {
            eprintln!("not run: trusted.* requires CAP_SYS_ADMIN for an ordinary user: {error}")
        }
        Err(error) => panic!("unexpected trusted.* probe failure: {error}"),
    }
}

/// A completed plan is a journal-only answer. Losing the installation root
/// must not turn a replay into `UnsupportedTarget` or touch the game tree.
#[cfg(unix)]
#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn dispatch_replays_a_claimed_plan_after_the_installation_root_is_gone() {
    let mut fixture = Fixture::new();
    let (plan, applied) = fixture.apply_one();
    assert_eq!(applied.phase, "completed");
    let deployed = tree(&fixture.game);
    let ops = fixture.app.join("effects/ops");
    let ops_before = entry_names(&ops);

    // Claim a Restore without running it, so the missing root must also answer
    // the journal-only restore route.
    let binding =
        marduk_platform::bind_installation(&fixture.game, &marduk_games::skyrim_se::LAYOUT)
            .unwrap();
    let (installation, data) = binding.identities();
    let (installation, data) = (installation.clone(), data.clone());
    drop(binding);
    let mut journal = OperationJournal::open(&fixture.app).unwrap();
    let generation = journal
        .root_state(&data)
        .unwrap()
        .active
        .expect("apply leaves an active generation");
    let pending = journal
        .claim(Claim::Restore {
            installation,
            data,
            generation,
        })
        .unwrap();

    let moved = fixture.temp.path().join("game-gone");
    fs::rename(&fixture.game, &moved).unwrap();

    let replay = expect_ok(fixture.send(json!({
        "op": "deploy_apply",
        "args": { "planId": plan.get().to_string() },
    })));
    assert_eq!(replay["id"], json!(applied.id));
    assert_eq!(replay["phase"], json!("completed"));
    assert_eq!(replay["blocker"], Value::Null);
    assert_eq!(tree(&moved), deployed);
    assert_eq!(entry_names(&ops), ops_before);

    let restore = expect_ok(fixture.send(json!({"op": "restore", "args": {}})));
    assert_eq!(restore["outcome"], json!("operation"));
    assert_eq!(restore["operation"]["id"], json!(pending.get().to_string()));
    assert_eq!(restore["operation"]["kind"], json!("restore"));
    assert_eq!(tree(&moved), deployed);
    assert_eq!(entry_names(&ops), ops_before);
}

/// The volume is still qualified, but the recorded pair no longer reopens:
/// availability must withdraw and the preview must refuse with a typed blocker
/// instead of returning a plan the write would fail to apply.
#[cfg(unix)]
#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn qualified_volume_with_a_replaced_root_advertises_no_effects() {
    let mut fixture = Fixture::new();
    let status = fixture.setup();
    let revision = status["profile"]["revision"].clone();

    // Same volume and the same supported layout spelling, different identity.
    let moved = fixture.temp.path().join("game-moved");
    fs::rename(&fixture.game, &moved).unwrap();
    fs::create_dir_all(fixture.game.join("Data/textures")).unwrap();
    fs::write(fixture.game.join("SkyrimSE.exe"), b"").unwrap();

    let status = expect_ok(fixture.send(json!({"op": "status", "args": {}})));
    assert_eq!(status["availability"]["deploy"]["available"], json!(false));
    assert_eq!(status["availability"]["restore"]["available"], json!(false));
    assert_eq!(
        status["availability"]["deploy"]["blocker"]["code"],
        json!("TargetUnavailable")
    );
    assert_eq!(
        status["availability"]["restore"]["blocker"]["code"],
        json!("TargetUnavailable")
    );

    let error = expect_error(fixture.send(json!({
        "op": "deploy_preview",
        "args": { "atRevision": revision },
    })));
    assert_eq!(error["code"], json!("TargetUnavailable"));
    let ops = fixture.app.join("effects/ops");
    assert!(
        !ops.exists() || fs::read_dir(&ops).unwrap().next().is_none(),
        "an unavailable root returns no preview and creates no operation-owned directory"
    );
}

#[cfg(unix)]
#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn unqualified_root_answers_a_claimed_plan_and_pending_restore() {
    let mut fixture = Fixture::new();
    let (plan, applied) = fixture.apply_one();
    assert_eq!(applied.phase, "completed");

    // Leave a claimed Restore in the journal without running it, then make any
    // new native port refuse the app root.
    let binding =
        marduk_platform::bind_installation(&fixture.game, &marduk_games::skyrim_se::LAYOUT)
            .unwrap();
    let (installation, data) = binding.identities();
    let (installation, data) = (installation.clone(), data.clone());
    drop(binding);
    let mut journal = OperationJournal::open(&fixture.app).unwrap();
    let generation = journal
        .root_state(&data)
        .unwrap()
        .active
        .expect("apply leaves an active generation");
    let pending = journal
        .claim(Claim::Restore {
            installation,
            data,
            generation,
        })
        .unwrap();
    let _real = fixture.make_root_unqualified();

    // The recorded plan and its terminal status come from the journal, not a
    // reopened volume.
    let replay = fixture.workspace.proof_apply_deployment(plan).unwrap();
    assert_eq!(replay.id, applied.id);
    assert_eq!(replay.phase, "completed");

    // The pending Restore is reported by id instead of `UnsupportedTarget`.
    let restore = fixture.workspace.proof_restore().unwrap();
    let RestoreResult::Operation { operation } = restore else {
        panic!("expected the recorded pending restore, got {restore:?}");
    };
    assert_eq!(operation.id, pending.get().to_string());
    assert!(matches!(operation.kind, Kind::Restore));
}

#[cfg(unix)]
#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn clean_unqualified_root_answers_before_opening_a_native_port() {
    let mut fixture = Fixture::new();
    fixture.apply_one();
    let restored = fixture.workspace.proof_restore().unwrap();
    assert!(matches!(
        restored,
        RestoreResult::Operation { operation } if operation.phase == "completed"
    ));
    let _real = fixture.make_root_unqualified();

    let restore = fixture.workspace.proof_restore().unwrap();
    assert!(matches!(
        restore,
        RestoreResult::NoActiveDeployment { code } if code == Code::NoActiveDeployment
    ));
    assert!(fixture.workspace.proof_recover_pending().unwrap().is_none());
}

#[cfg(unix)]
#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn dispatch_deploy_and_restore_through_the_whole_composition() {
    let mut fixture = Fixture::new();
    let status = fixture.setup();
    assert_eq!(status["availability"]["deploy"]["available"], json!(true));
    assert_eq!(status["availability"]["restore"]["available"], json!(true));
    assert_eq!(status["availability"]["deploy"]["blocker"], Value::Null);

    let (revision, _) = fixture.import_and_order();
    let staged_winner = fixture.staged_winner();
    set_mode(&staged_winner, STAGED_MODE);
    set_mtime(&staged_winner, stamped(STAGED_MTIME));

    let preview = expect_ok(fixture.send(json!({
        "op": "deploy_preview",
        "args": { "atRevision": revision.to_string() },
    })));
    assert_eq!(preview["blockers"], json!([]));
    let plan_id = preview["id"].as_str().expect("preview plan id").to_owned();
    let replaced = preview["files"]
        .as_array()
        .expect("preview files")
        .iter()
        .position(|file| file["needsBackup"] == json!(true))
        .expect("a replaced file");

    let operation = expect_ok(fixture.send(json!({
        "op": "deploy_apply",
        "args": { "planId": plan_id },
    })));
    assert_eq!(operation["kind"], json!("deploy"));
    assert_eq!(operation["phase"], json!("completed"));
    assert_eq!(operation["blocker"], Value::Null);
    let generation = operation["generationId"]
        .as_str()
        .expect("committed generation")
        .to_owned();
    let operation_id = operation["id"].as_str().expect("operation id").to_owned();

    // Bytes and scoped metadata: apply publishes the staged payload, not the
    // displaced original.
    let deployed_winner = fixture.game.join("Data/textures/winner.dds");
    assert_eq!(fs::read(&deployed_winner).unwrap(), POST);
    assert_eq!(
        fs::read(fixture.game.join("Data/textures/a-only.dds")).unwrap(),
        ONLY
    );
    assert_eq!(mode_of(&deployed_winner), STAGED_MODE);
    assert_eq!(mtime_of(&deployed_winner), STAGED_MTIME);

    // Retention: the status names the actually displaced original and the
    // sealed slot still holds its bytes and metadata.
    let displaced = operation["retained"]
        .as_array()
        .expect("retained refs")
        .iter()
        .find(|retained| {
            retained["ordinal"] == json!(replaced as u32)
                && retained["direction"] == json!("forward")
                && retained["slot"] == json!("displaced")
        })
        .expect("status names the displaced object");
    assert_eq!(displaced["operationId"], json!(operation_id));
    let forward = fixture
        .app
        .join("effects/ops")
        .join(&operation_id)
        .join(replaced.to_string())
        .join("forward");
    let sealed = if forward.join("displaced").exists() {
        forward.join("displaced")
    } else {
        forward.join("stage")
    };
    assert_eq!(fs::read(&sealed).unwrap(), BASE);
    assert_eq!(mode_of(&sealed), ORIGINAL_MODE);
    assert_eq!(mtime_of(&sealed), ORIGINAL_MTIME);

    // The committed generation survives a restart and is what a client reads
    // before enabling Restore.
    drop(fixture.workspace);
    fixture.workspace = Workspace::open(&fixture.app).unwrap();
    let status: WorkspaceStatus = serde_json::from_value(expect_ok(fixture.send(json!({
        "op": "status",
        "args": {},
    }))))
    .unwrap();
    assert!(status.pending.is_none());
    assert_eq!(status.active_generation_id, Some(generation));

    let restored = expect_ok(fixture.send(json!({ "op": "restore", "args": {} })));
    assert_eq!(restored["outcome"], json!("operation"));
    assert_eq!(restored["operation"]["kind"], json!("restore"));
    assert_eq!(restored["operation"]["phase"], json!("completed"));
    assert_eq!(restored["operation"]["blocker"], Value::Null);
    assert_eq!(restored["operation"]["generationId"], Value::Null);
    assert_eq!(tree(&fixture.game), fixture.initial);
    assert_eq!(fs::read(&deployed_winner).unwrap(), BASE);
    assert_eq!(mode_of(&deployed_winner), ORIGINAL_MODE);
    assert_eq!(mtime_of(&deployed_winner), ORIGINAL_MTIME);

    let second = expect_ok(fixture.send(json!({ "op": "restore", "args": {} })));
    assert_eq!(second["outcome"], json!("noActiveDeployment"));
    assert_eq!(second["code"], json!("NoActiveDeployment"));
    assert_eq!(tree(&fixture.game), fixture.initial);
}

#[cfg(unix)]
#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn unqualified_installation_root_keeps_client_deploy_and_restore_refused() {
    let mut fixture = Fixture::new();
    fixture.setup();

    // The gate asks the recorded installation root's volume class. Point that
    // path at a link the probe cannot resolve: the record has no proof for the
    // answer, so dispatch refuses before any effect. The fixture and its
    // initial tree stay intact behind the link.
    let real = fixture.temp.path().join("game-real");
    fs::rename(&fixture.game, &real).unwrap();
    std::os::unix::fs::symlink("/sys/kernel", &fixture.game).unwrap();

    let status = expect_ok(fixture.send(json!({"op": "status", "args": {}})));
    assert_eq!(status["availability"]["deploy"]["available"], json!(false));
    assert_eq!(
        status["availability"]["deploy"]["blocker"]["code"],
        json!("UnsupportedTarget")
    );
    assert_eq!(
        status["availability"]["deploy"]["blocker"]["detail"]["reason"],
        json!("Configuration")
    );

    for request in [
        json!({"op": "deploy_preview", "args": {"atRevision": "0"}}),
        json!({"op": "deploy_apply", "args": {"planId": "1"}}),
    ] {
        let error = expect_error(fixture.send(request));
        assert_eq!(error["code"], json!("UnsupportedTarget"));
        assert_eq!(error["detail"]["reason"], json!("Configuration"));
    }

    // A clean root has no recorded work, so Restore answers from the journal
    // before the gate; the refusal applies to new writes only.
    let restore = expect_ok(fixture.send(json!({"op": "restore", "args": {}})));
    assert_eq!(restore["outcome"], json!("noActiveDeployment"));
    assert_eq!(restore["code"], json!("NoActiveDeployment"));

    assert_eq!(tree(&real), fixture.initial);
    let ops = fixture.app.join("effects/ops");
    assert!(
        !ops.exists() || fs::read_dir(&ops).unwrap().next().is_none(),
        "a refused route creates no operation-owned directory"
    );
}

#[test]
#[ignore = "requires TMPDIR on tmpfs or Btrfs"]
fn mixed_case_archive_target_merges_with_the_existing_spelling() {
    let mut fixture = Fixture::new();
    fixture.setup();

    let lower = fixture.zip_records(
        "lower.zip",
        &[
            Record::file(b"textures/winner.dds", POST),
            Record::file(b"textures/a-only.dds", ONLY),
        ],
    );
    let mixed = fixture.zip_records("mixed.zip", &[Record::file(b"Textures/Winner.dds", MIXED)]);
    let first = fixture.import(&lower, "lower", "0");
    let at_revision = first["revision"].as_str().unwrap().to_owned();
    let second = fixture.import(&mixed, "mixed", &at_revision);

    // The profile now holds both spellings; the higher-priority archive's
    // spelling is the plan target. It must still name the one existing file,
    // not a second case-different path.
    let member_ids: Vec<Value> = second["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|member| member["id"].clone())
        .collect();
    assert_eq!(member_ids.len(), 2);
    let ordered = expect_ok(fixture.send(json!({
        "op": "order",
        "args": {
            "atRevision": second["revision"],
            "memberIds": member_ids,
        },
    })));
    let revision: u64 = ordered["revision"].as_str().unwrap().parse().unwrap();
    let preview = fixture
        .workspace
        .proof_preview_deployment(ProfileRevision::new(revision))
        .unwrap();
    assert_eq!(preview.files.len(), 2, "case aliases are one target");
    let winner = preview
        .files
        .iter()
        .find(|file| file.target.path.eq_ignore_ascii_case("textures/winner.dds"))
        .expect("the winner is one resolved target");
    assert_eq!(winner.target.path, "Textures/Winner.dds");
    assert!(winner.needs_backup, "the existing file is backed up");

    let plan = PlanId::try_from_u64(preview.id.as_ref().unwrap().parse().unwrap()).unwrap();
    let operation = fixture.workspace.proof_apply_deployment(plan).unwrap();
    assert_eq!(operation.phase, "completed");
    assert!(
        operation.blocker.is_none(),
        "apply blocker: {:?}",
        operation.blocker
    );

    let after = tree(&fixture.game);
    assert_eq!(after["Data/textures/winner.dds"], MIXED);
    assert_eq!(after["Data/textures/a-only.dds"], ONLY);
    assert!(
        !after.keys().any(|path| path == "Data/Textures/Winner.dds"),
        "the archive spelling must not create a second path"
    );

    let RestoreResult::Operation { operation } = fixture.workspace.proof_restore().unwrap() else {
        panic!("expected a tracked restore");
    };
    assert_eq!(operation.phase, "completed");
    assert_eq!(tree(&fixture.game), fixture.initial);
}
