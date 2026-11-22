//! Cross-platform private-library staging witnesses.
//!
//! These mirror the Linux `library_boundary` witnesses through the same public
//! adapter: real staged bytes, owner-only revision privacy, redirect refusal
//! with no writes outside the bound tree, and re-observation after reopening
//! the library.
//!
//! Windows runs them by default on an NTFS scratch volume. On Linux an
//! unqualified volume (ext4, native NTFS, FUSE) reports "not run" instead of
//! passing without assertions.

#![cfg(any(target_os = "linux", windows))]

#[allow(dead_code)]
#[path = "../../archive/tests/fixtures/mod.rs"]
mod zip_fixture;

use std::fs;
use std::path::{Path, PathBuf};

use marduk_core::ids::InstalledRevisionId;
use marduk_core::import::{ImportAllocation, ImportError, ImportId};
use marduk_platform::WriterGate;
use marduk_platform::library_stage::NativeStage;
use zip_fixture::{ClassicZip, Record};

const FILE: &str = "textures/synthetic.dds";
const BYTES: &[u8] = b"cross-platform private stage bytes";
const REVISIONS: &str = "revisions";

/// A scratch root on a volume where the platform admits private staging, or
/// `None` on Linux when the volume has no recorded library-staging evidence.
fn scratch() -> Option<tempfile::TempDir> {
    let tmp = tempfile::tempdir().expect("scratch directory");
    #[cfg(target_os = "linux")]
    if !marduk_platform::library_volume_qualified(tmp.path()) {
        eprintln!("not run: scratch volume is not qualified for private staging");
        return None;
    }
    Some(tmp)
}

fn library(tmp: &Path) -> PathBuf {
    let root = tmp.join("library");
    fs::create_dir(&root).unwrap();
    let revisions = root.join(REVISIONS);
    fs::create_dir(&revisions).unwrap();
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&revisions, fs::Permissions::from_mode(0o700)).unwrap();
    }
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

fn assert_refused(error: ImportError) {
    assert!(
        matches!(
            error,
            ImportError::StageChanged | ImportError::StageUnavailable
        ),
        "{error:?}"
    );
}

/// Create a directory redirection at `link`. Linux uses a symlink; Windows
/// tries a directory symlink and falls back to a junction, which an ordinary
/// user can create without the symlink privilege. `MARDUK_FORCE_JUNCTION=1`
/// forces the junction path so it can be exercised even in an elevated session.
fn link_directory(link: &Path, target: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
    #[cfg(windows)]
    {
        let force_junction = std::env::var("MARDUK_FORCE_JUNCTION")
            .map(|value| value != "0" && !value.is_empty())
            .unwrap_or(false);

        if !force_junction && std::os::windows::fs::symlink_dir(target, link).is_ok() {
            eprintln!("link_directory: used symlink_dir for {}", link.display());
            return true;
        }

        let created = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .status()
            .is_ok_and(|status| status.success());
        if created {
            eprintln!("link_directory: used junction for {}", link.display());
        }
        created
    }
}

#[test]
fn staged_revision_keeps_real_bytes_private_and_reobserves_after_reopen() {
    let Some(tmp) = scratch() else {
        return;
    };
    let root = library(tmp.path());
    let gate = WriterGate::open(root.join("lock")).unwrap();
    let guard = gate.try_lock().unwrap();
    let archive = zip();
    let mut input = archive.as_slice();
    let mut recorded = None;

    let receipt = {
        let mut stage = NativeStage::open(&root).unwrap();
        stage
            .stage(&guard, allocation(1), &mut input, &mut |owner| {
                recorded = Some(owner.clone());
                Ok(())
            })
            .unwrap()
    };

    assert_eq!(recorded.as_ref(), Some(&receipt.evidence().owner));
    let revision = root.join(REVISIONS).join("1");
    let payload = revision.join("payload");
    let textures = payload.join("textures");
    let staged_file = textures.join("synthetic.dds");
    for (path, is_directory) in [
        (&revision, true),
        (&payload, true),
        (&textures, true),
        (&staged_file, false),
    ] {
        assert!(
            marduk_platform::native_proof::staged_entry_is_private(path, is_directory),
            "{} must be private to the current user",
            path.display()
        );
    }
    #[cfg(windows)]
    {
        // Printed evidence for the reviewer: the independent scanner above is
        // the assertion, and `icacls` shows the protected owner-only DACL for
        // the directory and for the per-file descriptor explicitly.
        for path in [&revision, &staged_file] {
            let output = std::process::Command::new("cmd")
                .args(["/C", "icacls"])
                .arg(path)
                .output()
                .expect("run icacls");
            eprintln!(
                "icacls {}:\n{}",
                path.display(),
                String::from_utf8_lossy(&output.stdout)
            );
            assert!(output.status.success(), "icacls must succeed");
        }
    }
    assert!(revision.join("archive.zip").is_file());
    assert_eq!(
        fs::read(revision.join("payload").join(FILE)).unwrap(),
        BYTES
    );
    assert_eq!(receipt.evidence().files.len(), 1);
    assert_eq!(receipt.evidence().files[0].sha256.len(), 32);

    // A freshly bound adapter re-proves the same revision and payload.
    let mut reopened = NativeStage::open(&root).unwrap();
    reopened.reobserve(&guard, receipt.evidence()).unwrap();
}

#[test]
fn redirected_revisions_is_refused_without_writes() {
    let Some(tmp) = scratch() else {
        return;
    };
    let root = tmp.path().join("library");
    fs::create_dir(&root).unwrap();
    let outside = tmp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    #[cfg(windows)]
    std::env::set_var("MARDUK_FORCE_JUNCTION", "1");
    assert!(link_directory(&root.join(REVISIONS), &outside));
    #[cfg(windows)]
    std::env::remove_var("MARDUK_FORCE_JUNCTION");

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
    let mut stage = NativeStage::open(&root).unwrap();
    let outside = tmp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    assert!(link_directory(&root.join(REVISIONS).join("7"), &outside));
    let gate = WriterGate::open(root.join("lock")).unwrap();
    let guard = gate.try_lock().unwrap();

    let error = refused(&mut stage, &guard, 7);
    assert_refused(error);
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
}

#[test]
fn redirected_payload_during_owner_record_is_refused_without_writes() {
    let Some(tmp) = scratch() else {
        return;
    };
    let root = library(tmp.path());
    let mut stage = NativeStage::open(&root).unwrap();
    let gate = WriterGate::open(root.join("lock")).unwrap();
    let guard = gate.try_lock().unwrap();
    let outside = tmp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let archive = zip();
    let mut input = archive.as_slice();

    let error = match stage.stage(&guard, allocation(1), &mut input, &mut |_owner| {
        assert!(link_directory(
            &root.join(REVISIONS).join("1").join("payload"),
            &outside
        ));
        Ok(())
    }) {
        Ok(_) => panic!("redirected payload during owner recording must be refused"),
        Err(error) => error,
    };

    assert_refused(error);
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
}

#[test]
fn reclaim_removes_a_verified_private_stage() {
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

    stage.reclaim(&guard, receipt.evidence()).unwrap();
    assert!(!root.join(REVISIONS).join("1").exists());
    assert_eq!(fs::read_dir(root.join(REVISIONS)).unwrap().count(), 0);
}

#[test]
fn reobserve_refuses_a_changed_bound_entry() {
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
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&revision, fs::Permissions::from_mode(0o755)).unwrap();
    }
    #[cfg(windows)]
    {
        // Add an explicit Everyone ACE (locale-independent SID spelling) so the
        // protected owner-only DACL is no longer owner-only.
        let status = std::process::Command::new("cmd")
            .args(["/C", "icacls"])
            .arg(&revision)
            .arg("/grant")
            .arg("*S-1-1-0:(OI)(CI)F")
            .status()
            .expect("run icacls");
        assert!(status.success(), "granting Everyone must succeed");
    }

    assert_eq!(
        stage.reobserve(&guard, receipt.evidence()).unwrap_err(),
        ImportError::StageChanged
    );
}

#[test]
fn reobserve_refuses_an_added_payload_file() {
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

    let extra = root
        .join(REVISIONS)
        .join("1")
        .join("payload")
        .join("textures")
        .join("extra.dds");
    fs::write(&extra, b"added after the stage").unwrap();

    assert_eq!(
        stage.reobserve(&guard, receipt.evidence()).unwrap_err(),
        ImportError::StageChanged
    );
}

#[test]
fn reobserve_refuses_a_revision_redirected_while_unbound() {
    let Some(tmp) = scratch() else {
        return;
    };
    let root = library(tmp.path());
    let gate = WriterGate::open(root.join("lock")).unwrap();
    let guard = gate.try_lock().unwrap();
    let archive = zip();
    let mut input = archive.as_slice();
    let receipt = {
        let mut stage = NativeStage::open(&root).unwrap();
        stage
            .stage(&guard, allocation(1), &mut input, &mut |_owner| Ok(()))
            .unwrap()
    };

    let outside = tmp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let real = tmp.path().join("real-revision");
    fs::rename(root.join(REVISIONS).join("1"), &real).unwrap();
    assert!(link_directory(&root.join(REVISIONS).join("1"), &outside));

    let mut stage = NativeStage::open(&root).unwrap();
    assert_refused(stage.reobserve(&guard, receipt.evidence()).unwrap_err());
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    assert!(real.join("archive.zip").is_file());
}
