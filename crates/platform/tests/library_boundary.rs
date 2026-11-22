//! Focused private-library boundary tests for native staging.
//!
//! Native staging is Btrfs-only. Tests use `MARDUK_M1_BTRFS_SCRATCH` when set,
//! else `/var/tmp` if it is Btrfs, and skip on an unqualified volume rather than
//! claiming a native pass.

#![cfg(target_os = "linux")]

#[allow(dead_code)]
#[path = "../../archive/tests/fixtures/mod.rs"]
mod zip_fixture;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use marduk_core::ids::InstalledRevisionId;
use marduk_core::import::{ImportAllocation, ImportError, ImportId};
use marduk_platform::WriterGate;
use marduk_platform::library_stage::NativeStage;
use zip_fixture::{ClassicZip, Record};

const FILE: &str = "textures/synthetic.dds";
const BYTES: &[u8] = b"private stage bytes";
const REVISIONS: &str = "revisions";

fn scratch() -> Option<tempfile::TempDir> {
    let parent = match std::env::var_os("MARDUK_M1_BTRFS_SCRATCH") {
        Some(parent) => PathBuf::from(parent),
        None => {
            let output = Command::new("stat")
                .args(["-f", "-c", "%T", "/var/tmp"])
                .output()
                .ok()?;
            if String::from_utf8_lossy(&output.stdout).trim() != "btrfs" {
                return None;
            }
            PathBuf::from("/var/tmp")
        }
    };
    Some(
        tempfile::Builder::new()
            .prefix("marduk-stage-boundary-")
            .tempdir_in(parent)
            .unwrap(),
    )
}

fn library(tmp: &Path) -> PathBuf {
    let root = tmp.join("library");
    fs::create_dir(&root).unwrap();
    let revisions = root.join(REVISIONS);
    fs::create_dir(&revisions).unwrap();
    fs::set_permissions(&revisions, fs::Permissions::from_mode(0o700)).unwrap();
    root
}

fn allocation(id: u64) -> ImportAllocation {
    ImportAllocation {
        id: ImportId::try_from_u64(id).unwrap(),
        revision: InstalledRevisionId::new(id),
    }
}

fn zip() -> Vec<u8> {
    ClassicZip::new(&[Record::file(FILE.as_bytes(), BYTES)]).bytes
}

fn refused(stage: &mut NativeStage, guard: &marduk_platform::WriterGuard, id: u64) -> ImportError {
    let archive = zip();
    let mut input = archive.as_slice();
    match stage.stage(guard, allocation(id), &mut input, &mut |_owner| Ok(())) {
        Ok(_) => panic!("redirected staging must be refused"),
        Err(error) => error,
    }
}

#[test]
fn symlinked_revisions_is_refused_without_writes() {
    let Some(tmp) = scratch() else {
        return;
    };
    let root = tmp.path().join("library");
    fs::create_dir(&root).unwrap();
    let outside = tmp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, root.join(REVISIONS)).unwrap();

    assert!(matches!(
        NativeStage::open(&root),
        Err(ImportError::StageUnavailable)
    ));
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
}

#[test]
fn redirected_revision_path_is_refused_without_writes() {
    let Some(tmp) = scratch() else {
        return;
    };
    let root = library(tmp.path());
    let outside = tmp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let mut stage = NativeStage::open(&root).unwrap();
    std::os::unix::fs::symlink(&outside, root.join(REVISIONS).join("7")).unwrap();
    let gate = WriterGate::open(root.join("lock")).unwrap();
    let guard = gate.try_lock().unwrap();

    let error = refused(&mut stage, &guard, 7);
    assert!(
        matches!(
            error,
            ImportError::StageChanged | ImportError::StageUnavailable
        ),
        "{error:?}"
    );
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
}

#[test]
fn redirected_revisions_after_open_is_rechecked_before_write() {
    let Some(tmp) = scratch() else {
        return;
    };
    let root = library(tmp.path());
    let mut stage = NativeStage::open(&root).unwrap();
    let outside = tmp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let real = tmp.path().join("real-revisions");
    fs::rename(root.join(REVISIONS), &real).unwrap();
    std::os::unix::fs::symlink(&outside, root.join(REVISIONS)).unwrap();
    let gate = WriterGate::open(root.join("lock")).unwrap();
    let guard = gate.try_lock().unwrap();

    let error = refused(&mut stage, &guard, 1);
    assert_eq!(error, ImportError::StageChanged);
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    assert_eq!(fs::read_dir(&real).unwrap().count(), 0);
}

#[test]
fn redirect_during_owner_record_is_refused_before_payload_write() {
    let Some(tmp) = scratch() else {
        return;
    };
    let root = library(tmp.path());
    let mut stage = NativeStage::open(&root).unwrap();
    let gate = WriterGate::open(root.join("lock")).unwrap();
    let guard = gate.try_lock().unwrap();
    let outside = tmp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let real = tmp.path().join("real-revisions");
    let archive = zip();
    let mut input = archive.as_slice();

    let error = match stage.stage(&guard, allocation(1), &mut input, &mut |_owner| {
        fs::rename(root.join(REVISIONS), &real).unwrap();
        std::os::unix::fs::symlink(&outside, root.join(REVISIONS)).unwrap();
        Ok(())
    }) {
        Ok(_) => panic!("redirect during owner recording must be refused"),
        Err(error) => error,
    };

    assert_eq!(error, ImportError::StageChanged);
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    let revision = real.join("1");
    assert!(revision.is_dir());
    assert!(!revision.join("archive.zip").exists());
}

#[test]
fn valid_private_root_stages_and_reobserves() {
    let Some(tmp) = scratch() else {
        return;
    };
    let root = library(tmp.path());
    let mut stage = NativeStage::open(&root).unwrap();
    let gate = WriterGate::open(root.join("lock")).unwrap();
    let guard = gate.try_lock().unwrap();
    let archive = zip();
    let mut input = archive.as_slice();
    let mut recorded = None;
    let receipt = stage
        .stage(&guard, allocation(1), &mut input, &mut |owner| {
            recorded = Some(owner.clone());
            Ok(())
        })
        .unwrap();

    assert_eq!(recorded.as_ref(), Some(&receipt.evidence().owner));
    let revision = root.join(REVISIONS).join("1");
    assert!(revision.join("archive.zip").is_file());
    assert_eq!(
        fs::read(revision.join("payload").join(FILE)).unwrap(),
        BYTES
    );
    assert_eq!(receipt.evidence().files.len(), 1);
    stage.reobserve(&guard, receipt.evidence()).unwrap();
}

#[test]
fn reobserve_detects_a_changed_bound_entry_after_flush() {
    let Some(tmp) = scratch() else {
        return;
    };
    let root = library(tmp.path());
    let mut stage = NativeStage::open(&root).unwrap();
    let gate = WriterGate::open(root.join("lock")).unwrap();
    let guard = gate.try_lock().unwrap();
    let archive = zip();
    let mut input = archive.as_slice();
    let receipt = stage
        .stage(&guard, allocation(1), &mut input, &mut |_owner| Ok(()))
        .unwrap();

    let revision = root.join(REVISIONS).join("1");
    fs::set_permissions(&revision, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        stage.reobserve(&guard, receipt.evidence()).unwrap_err(),
        ImportError::StageChanged
    );
}
