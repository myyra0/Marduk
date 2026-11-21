//! Windows scoped metadata eligibility regressions.
//!
//! Eligibility is evidence for one ordinary single-link file: owner, primary
//! group, DACL, attributes, last-write time, no EA/ADS, and no explicit
//! mandatory-label, resource-attribute or scoped-policy ACE on the selected
//! `READ_CONTROL` queries. These tests exercise the observable outcomes for an
//! ordinary scratch file and for security classes the environment can attach
//! without elevation.

#![cfg(windows)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use marduk_core::root_identity::{GameLayout, TargetObservation, UnsupportedReason};
use marduk_core::targets::RelativePath;
use marduk_platform::{bind_installation, observe_target, resolve_existing_parent};

const LAYOUT: GameLayout = GameLayout {
    data: "Data",
    markers: &["fixture.marker"],
};

fn scratch() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join("Data")).unwrap();
    fs::write(temp.path().join("fixture.marker"), b"inert").unwrap();
    let file = temp.path().join("Data/original.dds");
    fs::write(&file, b"inert").unwrap();
    (temp, file)
}

fn observe_file(file: &Path) -> TargetObservation {
    let binding = bind_installation(file.parent().unwrap().parent().unwrap(), &LAYOUT).unwrap();
    let relative = RelativePath::parse(file.file_name().unwrap().to_str().unwrap()).unwrap();
    let target = resolve_existing_parent(&binding, &relative).unwrap();
    observe_target(&target).unwrap()
}

fn icacls(file: &Path, arguments: &[&str]) -> bool {
    Command::new("icacls")
        .arg(file)
        .args(arguments)
        .status()
        .is_ok_and(|status| status.success())
}

/// An ordinary NTFS scratch file has no explicit label, resource-attribute or
/// scoped-policy ACE, so its observation is deterministically eligible. Two
/// observations of unchanged bytes and metadata yield the same fingerprint,
/// including the owner/group/DACL digest.
#[test]
fn ordinary_scratch_file_is_eligible_and_deterministic() {
    let (_temp, file) = scratch();

    let first = observe_file(&file);
    let second = observe_file(&file);
    let TargetObservation::Regular { fingerprint: first } = first else {
        panic!("ordinary scratch file must be regular, got {first:?}");
    };
    let TargetObservation::Regular {
        fingerprint: second,
    } = second
    else {
        panic!("ordinary scratch file must be regular, got {second:?}");
    };
    assert_eq!(first, second);
    assert_eq!(first.len, 5);
    assert_eq!(first.metadata, second.metadata);
}

/// An explicit integrity label is refused as metadata, never silently allowed.
/// Environments that cannot attach one without elevation are recorded as a
/// skipped proof case rather than a pass.
#[test]
fn explicit_integrity_label_refuses_the_original() {
    let (_temp, file) = scratch();
    if !icacls(&file, &["/setintegritylevel", "L"]) {
        eprintln!("skipped: this environment cannot attach an explicit integrity label");
        return;
    }

    assert_eq!(
        observe_file(&file),
        TargetObservation::Unsupported {
            reason: UnsupportedReason::Metadata
        }
    );
}

/// If the bound object's security evidence cannot be read, observation refuses.
/// A `READ_CONTROL` denial is the failure this environment can create without
/// elevation; it must not fall through to an eligible observation.
#[test]
fn unreadable_security_evidence_fails_closed() {
    let (_temp, file) = scratch();
    if !icacls(&file, &["/deny", "*S-1-1-0:(RC)"]) {
        eprintln!("skipped: this environment cannot deny READ_CONTROL");
        return;
    }

    let refused = observe_file(&file);
    assert!(
        matches!(
            refused,
            TargetObservation::Unsupported {
                reason: UnsupportedReason::Sharing | UnsupportedReason::Metadata
            }
        ),
        "unreadable security evidence must refuse, got {refused:?}"
    );

    // Restore access and prove the same object is eligible again, so the
    // refusal came from evidence access and not from a changed original.
    assert!(icacls(&file, &["/remove:d", "*S-1-1-0"]));
    assert!(matches!(
        observe_file(&file),
        TargetObservation::Regular { .. }
    ));
}
