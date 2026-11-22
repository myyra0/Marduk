//! Focused private-library boundary tests for native staging.
//!
//! Native staging is Btrfs-only. Every test is ignored unless run explicitly and
//! `MARDUK_M1_BTRFS_SCRATCH` names a scratch directory on Btrfs; an unconfigured
//! run therefore reports them as ignored instead of passing without assertions.

#![cfg(target_os = "linux")]

#[allow(dead_code)]
#[path = "../../archive/tests/fixtures/mod.rs"]
mod zip_fixture;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use marduk_core::ids::InstalledRevisionId;
use marduk_core::import::{ImportAllocation, ImportError, ImportId};
use marduk_platform::WriterGate;
use marduk_platform::library_stage::NativeStage;
use zip_fixture::{ClassicZip, Record};

const FILE: &str = "textures/synthetic.dds";
const BYTES: &[u8] = b"private stage bytes";
const REVISIONS: &str = "revisions";

fn scratch() -> tempfile::TempDir {
    let parent = PathBuf::from(
        std::env::var_os("MARDUK_M1_BTRFS_SCRATCH")
            .expect("MARDUK_M1_BTRFS_SCRATCH must name a Btrfs scratch directory"),
    );
    tempfile::Builder::new()
        .prefix("marduk-stage-boundary-")
        .tempdir_in(parent)
        .expect("Btrfs scratch directory is usable")
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
#[ignore = "requires MARDUK_M1_BTRFS_SCRATCH"]
fn symlinked_revisions_is_refused_without_writes() {
    let tmp = scratch();
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
#[ignore = "requires MARDUK_M1_BTRFS_SCRATCH"]
fn redirected_revision_path_is_refused_without_writes() {
    let tmp = scratch();
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
#[ignore = "requires MARDUK_M1_BTRFS_SCRATCH"]
fn redirected_revisions_after_open_is_rechecked_before_write() {
    let tmp = scratch();
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
#[ignore = "requires MARDUK_M1_BTRFS_SCRATCH"]
fn redirect_during_owner_record_is_refused_before_payload_write() {
    let tmp = scratch();
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
#[ignore = "requires MARDUK_M1_BTRFS_SCRATCH"]
fn valid_private_root_stages_and_reobserves() {
    let tmp = scratch();
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
    assert_eq!(
        fs::metadata(&revision).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert!(revision.join("archive.zip").is_file());
    assert_eq!(
        fs::read(revision.join("payload").join(FILE)).unwrap(),
        BYTES
    );
    assert_eq!(receipt.evidence().files.len(), 1);
    stage.reobserve(&guard, receipt.evidence()).unwrap();
}

#[test]
#[ignore = "requires MARDUK_M1_BTRFS_SCRATCH"]
fn reobserve_detects_a_changed_bound_entry_after_flush() {
    let tmp = scratch();
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

#[test]
#[ignore = "requires MARDUK_M1_BTRFS_SCRATCH"]
fn reobserve_refuses_a_revision_redirected_after_stage_open() {
    let tmp = scratch();
    let root = library(tmp.path());
    let mut stage = NativeStage::open(&root).unwrap();
    let gate = WriterGate::open(root.join("lock")).unwrap();
    let guard = gate.try_lock().unwrap();
    let archive = zip();
    let mut input = archive.as_slice();
    let receipt = stage
        .stage(&guard, allocation(1), &mut input, &mut |_owner| Ok(()))
        .unwrap();

    let outside = tmp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let real = tmp.path().join("real-revision");
    fs::rename(root.join(REVISIONS).join("1"), &real).unwrap();
    std::os::unix::fs::symlink(&outside, root.join(REVISIONS).join("1")).unwrap();

    let error = stage.reobserve(&guard, receipt.evidence()).unwrap_err();
    assert!(
        matches!(
            error,
            ImportError::StageChanged | ImportError::StageUnavailable
        ),
        "{error:?}"
    );
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    assert!(real.join("archive.zip").is_file());
}

#[test]
#[ignore = "requires MARDUK_M1_BTRFS_SCRATCH"]
fn reclaim_refuses_a_revision_redirected_after_stage_open() {
    let tmp = scratch();
    let root = library(tmp.path());
    let mut stage = NativeStage::open(&root).unwrap();
    let gate = WriterGate::open(root.join("lock")).unwrap();
    let guard = gate.try_lock().unwrap();
    let archive = zip();
    let mut input = archive.as_slice();
    let receipt = stage
        .stage(&guard, allocation(1), &mut input, &mut |_owner| Ok(()))
        .unwrap();

    let outside = tmp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let real = tmp.path().join("real-revision");
    fs::rename(root.join(REVISIONS).join("1"), &real).unwrap();
    std::os::unix::fs::symlink(&outside, root.join(REVISIONS).join("1")).unwrap();

    let error = stage.reclaim(&guard, receipt.evidence()).unwrap_err();
    assert!(
        matches!(
            error,
            ImportError::StageChanged | ImportError::StageUnavailable
        ),
        "{error:?}"
    );
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    assert!(real.join("archive.zip").is_file());
    assert!(real.join("payload").join(FILE).is_file());
}
