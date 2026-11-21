#[allow(dead_code)]
#[path = "../../../../crates/archive/tests/fixtures/mod.rs"]
mod zip_fixture;

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use marduk_core::ids::{ProfileId, ProfileRevision};
use marduk_core::import::{BeginImport, ImportError, ImportId, ImportTarget};
use marduk_storage::Catalog;
use zip_fixture::{ClassicZip, Record};

const PAYLOAD: &[u8] = b"immutable reviewed payload\n";

fn plan() -> BeginImport {
    BeginImport {
        profile: ProfileId::new(1),
        expected_revision: ProfileRevision::new(0),
        target: ImportTarget::Append {
            display_name: "Synthetic".into(),
        },
    }
}

fn archive() -> Vec<u8> {
    ClassicZip::new(&[Record::file(b"nested/fixture.txt", PAYLOAD)]).bytes
}

fn open(dir: &Path) -> Catalog {
    Catalog::open(dir).unwrap()
}

fn payload(dir: &Path, id: ImportId) -> PathBuf {
    dir.join("revisions")
        .join(id.get().to_string())
        .join("payload/nested/fixture.txt")
}

fn id(path: &Path) -> ImportId {
    ImportId::try_from_u64(fs::read_to_string(path).unwrap().parse().unwrap()).unwrap()
}

fn assert_empty(app: &Path) {
    let snapshot = open(app).snapshot(ProfileId::new(1)).unwrap();
    assert_eq!(snapshot.revision, ProfileRevision::new(0));
    assert!(snapshot.members.is_empty());
}

fn pause(dir: &Path) -> ! {
    File::options()
        .create_new(true)
        .write(true)
        .open(dir.join("ready"))
        .unwrap();
    loop {
        thread::park();
    }
}

struct CutRead<'a> {
    dir: &'a Path,
    sent: bool,
}

impl Read for CutRead<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if !self.sent {
            self.sent = true;
            out[0] = b'P';
            return Ok(1);
        }
        pause(self.dir)
    }
}

fn worker(dir: &Path, mode: &str) -> ! {
    let app = dir.join("app");
    let mut c = open(&app);
    match mode {
        "stream" => {
            let result = c.prepare_import(CutRead { dir, sent: false }, plan());
            panic!("stream did not reach its cut: {result:?}");
        }
        "prepared" => {
            let stage = c.prepare_import(archive().as_slice(), plan()).unwrap();
            fs::write(dir.join("id"), stage.id.get().to_string()).unwrap();
        }
        "committed" => {
            let selected = dir.join("selected.zip");
            fs::write(&selected, archive()).unwrap();
            let stage = c
                .prepare_import(File::open(&selected).unwrap(), plan())
                .unwrap();
            fs::write(selected, b"not the reviewed ZIP").unwrap();
            let result = c.publish_import(stage.id).unwrap();
            assert_eq!(result.members.len(), 1);
            fs::write(dir.join("id"), stage.id.get().to_string()).unwrap();
        }
        _ => panic!("unknown cut"),
    }
    pause(dir)
}

fn cut(dir: &Path, mode: &str) {
    fs::create_dir(dir).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .arg(dir)
        .arg(mode)
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let until = Instant::now() + Duration::from_secs(30);
    while !dir.join("ready").is_file() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "child exited before {mode} cut"
        );
        if Instant::now() >= until {
            child.kill().unwrap();
            let _ = child.wait();
            panic!("timed out at {mode} cut");
        }
        thread::sleep(Duration::from_millis(10));
    }
    child.kill().unwrap();
    assert!(!child.wait().unwrap().success());
    eprintln!("R03 real catalog child cut: {mode}");
}

fn verify(root: &Path) {
    let canary = root.join("canary");
    let readiness = open(&canary).prepare_import(archive().as_slice(), plan());
    match readiness {
        Ok(_) => {}
        Err(ImportError::StageUnavailable) => {
            assert_empty(&canary);
            eprintln!("R03 NOT RUN: catalog native receipt unavailable on this volume");
            return;
        }
        Err(other) => panic!("unexpected native stage readiness: {other:?}"),
    }

    let streaming = root.join("streaming");
    cut(&streaming, "stream");
    let app = streaming.join("app");
    assert_empty(&app);
    assert!(open(&app).reconcile().unwrap().is_empty());

    let prepared = root.join("prepared");
    cut(&prepared, "prepared");
    let app = prepared.join("app");
    let staged = id(&prepared.join("id"));
    assert_empty(&app);
    assert_eq!(open(&app).reconcile().unwrap(), [staged]);
    assert_eq!(fs::read(payload(&app, staged)).unwrap(), PAYLOAD);
    // Same length but different content; archive and catalog row remain intact.
    let staged_path = payload(&app, staged);
    fs::write(&staged_path, vec![b'!'; PAYLOAD.len()]).unwrap();
    assert_eq!(
        open(&app).publish_import(staged).unwrap_err(),
        ImportError::StageChanged
    );
    assert_eq!(
        open(&app).discard_import(staged),
        Err(ImportError::StageChanged)
    );
    assert_eq!(fs::read(&staged_path).unwrap(), vec![b'!'; PAYLOAD.len()]);
    assert_empty(&app);

    let missing = root.join("missing");
    cut(&missing, "prepared");
    let app = missing.join("app");
    let staged = id(&missing.join("id"));
    let held = missing.join("held-payload");
    fs::rename(payload(&app, staged), &held).unwrap();
    assert!(matches!(
        open(&app).publish_import(staged),
        Err(ImportError::StageChanged | ImportError::StageUnavailable)
    ));
    assert_empty(&app);
    assert_eq!(fs::read(held).unwrap(), PAYLOAD);

    let committed = root.join("committed");
    cut(&committed, "committed");
    let app = committed.join("app");
    let staged = id(&committed.join("id"));
    let path = payload(&app, staged);
    let before = fs::metadata(&path).unwrap().modified().unwrap();
    assert_eq!(fs::read(&path).unwrap(), PAYLOAD);
    let mut c = open(&app);
    let snapshot = c.snapshot(ProfileId::new(1)).unwrap();
    assert_eq!(snapshot.revision, ProfileRevision::new(1));
    assert_eq!(snapshot.members.len(), 1);
    assert_eq!(snapshot.members[0].installed.get(), staged.get());
    let replay = c.publish_import(staged).unwrap();
    assert_eq!(replay.id, snapshot.id);
    assert_eq!(replay.revision, snapshot.revision);
    assert_eq!(replay.members.len(), 1);
    let (actual, expected) = (&replay.members[0], &snapshot.members[0]);
    assert_eq!(
        (actual.id, actual.installed, actual.enabled),
        (expected.id, expected.installed, expected.enabled)
    );
    assert_eq!(fs::read(&path).unwrap(), PAYLOAD);
    assert_eq!(fs::metadata(path).unwrap().modified().unwrap(), before);
}

fn main() {
    let mut args = std::env::args_os().skip(1);
    let dir = PathBuf::from(args.next().unwrap());
    if let Some(mode) = args.next() {
        worker(&dir, mode.to_str().unwrap());
    }
    verify(&dir)
}
