//! Direct witnesses for the native effect primitives.
//!
//! The same cases run on both qualified hosts. They use only disposable files in
//! a temporary directory and assert the file-level contract: bytes, retained
//! identity and metadata, and refusal to clobber. No case claims power-cut
//! durability; Windows namespace persistence is not qualified by these tests.

#![cfg(any(target_os = "linux", windows))]

use std::fs::{self, File};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use marduk_core::root_identity::RootError;
use marduk_platform::effects::{
    copy_file, detach, fingerprint_path, open_directory, publish, replace, sync_directory,
    volume_qualified,
};

const STAGED: &[u8] = b"staged bytes\n";
const ORIGINAL: &[u8] = b"original bytes\n";
const UNKNOWN: &[u8] = b"user edit\n";

fn modified() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_600_000_000) + Duration::from_millis(1_234)
}

fn set_modified(path: &Path, time: SystemTime) {
    let file = File::options().write(true).open(path).unwrap();
    file.set_times(fs::FileTimes::new().set_modified(time))
        .unwrap();
}

#[test]
fn copy_preserves_bytes_and_last_write_time_and_refuses_existing() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    let destination = directory.path().join("destination");
    fs::write(&source, STAGED).unwrap();
    set_modified(&source, modified());
    let source_fingerprint = fingerprint_path(&source).unwrap();

    let (len, sha256) = copy_file(&source, &destination).unwrap();
    assert_eq!(len, STAGED.len() as u64);
    assert_eq!(fs::read(&destination).unwrap(), STAGED);

    let copy_fingerprint = fingerprint_path(&destination).unwrap();
    assert_eq!(copy_fingerprint.len, source_fingerprint.len);
    assert_eq!(copy_fingerprint.sha256, source_fingerprint.sha256);
    assert_eq!(copy_fingerprint.sha256, sha256);
    // The caller compares this metadata witness against the source. A data-only
    // copy would pass the digest and fail here.
    assert_eq!(copy_fingerprint.metadata, source_fingerprint.metadata);
    assert_eq!(
        fs::metadata(&destination).unwrap().modified().unwrap(),
        fs::metadata(&source).unwrap().modified().unwrap()
    );

    let error = copy_file(&source, &destination).expect_err("an existing destination is refused");
    assert!(
        matches!(error, RootError::UnsupportedTarget(_)),
        "unexpected refusal {error:?}"
    );
    assert_eq!(fs::read(&destination).unwrap(), STAGED);
}

#[test]
fn publish_refuses_an_existing_target_and_leaves_it_intact() {
    let directory = tempfile::tempdir().unwrap();
    let stage_directory = directory.path().join("stage-directory");
    let target_directory = directory.path().join("target-directory");
    fs::create_dir(&stage_directory).unwrap();
    fs::create_dir(&target_directory).unwrap();
    let stage_path = stage_directory.join("stage");
    let target_path = target_directory.join("leaf");
    let stage = open_directory(&stage_directory).unwrap();
    let target = open_directory(&target_directory).unwrap();

    fs::write(&stage_path, STAGED).unwrap();
    publish(&stage, "stage", &target, "leaf", &stage_path, &target_path).unwrap();
    assert_eq!(fs::read(&target_path).unwrap(), STAGED);
    assert!(!stage_path.exists());

    fs::write(&stage_path, b"second candidate\n").unwrap();
    let staged = fingerprint_path(&stage_path).unwrap();
    assert!(
        publish(&stage, "stage", &target, "leaf", &stage_path, &target_path,).is_err(),
        "publishing over an existing target must fail"
    );
    assert_eq!(fs::read(&target_path).unwrap(), STAGED);
    assert_eq!(fingerprint_path(&stage_path).unwrap(), staged);
}

#[test]
fn replace_retains_the_displaced_file_and_keeps_the_staged_object() {
    let directory = tempfile::tempdir().unwrap();
    let stage_directory = directory.path().join("stage-directory");
    let target_directory = directory.path().join("target-directory");
    fs::create_dir(&stage_directory).unwrap();
    fs::create_dir(&target_directory).unwrap();
    let stage_path = stage_directory.join("stage");
    let target_path = target_directory.join("leaf");
    let stage = open_directory(&stage_directory).unwrap();
    let target = open_directory(&target_directory).unwrap();

    fs::write(&stage_path, STAGED).unwrap();
    set_modified(&stage_path, modified());
    fs::write(&target_path, ORIGINAL).unwrap();
    set_modified(&target_path, modified() - Duration::from_secs(60));
    let staged = fingerprint_path(&stage_path).unwrap();
    let original = fingerprint_path(&target_path).unwrap();

    // The callers name the stage slot as the canonical retain alias.
    replace(
        &stage,
        "stage",
        &target,
        "leaf",
        &stage_path,
        &target_path,
        &stage_path,
    )
    .unwrap();

    assert_eq!(fs::read(&target_path).unwrap(), STAGED);
    assert_eq!(
        fingerprint_path(&target_path).unwrap(),
        staged,
        "the public target must be the exact staged object"
    );
    assert_eq!(fs::read(&stage_path).unwrap(), ORIGINAL);
    assert_eq!(
        fingerprint_path(&stage_path).unwrap(),
        original,
        "the actually displaced object must retain its identity and metadata"
    );
}

#[test]
fn detach_preserves_an_unknown_file() {
    let directory = tempfile::tempdir().unwrap();
    let target_directory = directory.path().join("target-directory");
    let retain_directory = directory.path().join("retain-directory");
    fs::create_dir(&target_directory).unwrap();
    fs::create_dir(&retain_directory).unwrap();
    let target_path = target_directory.join("leaf");
    let retain_path = retain_directory.join("displaced");
    let target = open_directory(&target_directory).unwrap();
    let retain = open_directory(&retain_directory).unwrap();

    fs::write(&target_path, UNKNOWN).unwrap();
    set_modified(&target_path, modified());
    let unknown = fingerprint_path(&target_path).unwrap();

    detach(
        &target,
        "leaf",
        &retain,
        "displaced",
        &target_path,
        &retain_path,
    )
    .unwrap();

    assert!(!target_path.exists());
    assert_eq!(fs::read(&retain_path).unwrap(), UNKNOWN);
    assert_eq!(
        fingerprint_path(&retain_path).unwrap(),
        unknown,
        "detach must move the unknown object, not copy or delete it"
    );

    fs::write(&target_path, ORIGINAL).unwrap();
    assert!(
        detach(
            &target,
            "leaf",
            &retain,
            "displaced",
            &target_path,
            &retain_path,
        )
        .is_err(),
        "an occupied retain slot must make detach fail"
    );
    assert_eq!(fs::read(&target_path).unwrap(), ORIGINAL);
    assert_eq!(fs::read(&retain_path).unwrap(), UNKNOWN);
}

#[test]
fn sync_directory_succeeds_on_a_real_directory() {
    let directory = tempfile::tempdir().unwrap();
    let child = directory.path().join("child");
    fs::create_dir(&child).unwrap();
    sync_directory(&child).unwrap();
    assert_eq!(
        sync_directory(&directory.path().join("absent")),
        Err(RootError::TargetUnavailable)
    );
}

#[test]
fn volume_qualification_reads_the_opened_root() {
    let directory = tempfile::tempdir().unwrap();
    assert!(!volume_qualified(&directory.path().join("absent")));

    #[cfg(windows)]
    {
        match filesystem_name(directory.path()).as_deref() {
            Some(name) if name.eq_ignore_ascii_case("NTFS") => assert!(
                volume_qualified(directory.path()),
                "an NTFS root must be qualified"
            ),
            Some(name) => {
                // exFAT/FAT32/ReFS volumes cannot be constructed portably here.
                eprintln!(
                    "skipped: the temporary directory is on {name}, not NTFS; \
                     no non-NTFS volume can be constructed on this host"
                );
                assert!(!volume_qualified(directory.path()));
            }
            None => panic!("the temporary directory has no readable volume name"),
        }
    }
}

#[cfg(windows)]
fn filesystem_name(path: &Path) -> Option<String> {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, GetVolumeInformationByHandleW,
    };

    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .ok()?;
    let mut name = [0u16; 64];
    let result = unsafe {
        GetVolumeInformationByHandleW(
            file.as_raw_handle(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            name.as_mut_ptr(),
            name.len() as u32,
        )
    };
    if result == 0 {
        return None;
    }
    let end = name.iter().position(|unit| *unit == 0)?;
    String::from_utf16(&name[..end]).ok()
}
