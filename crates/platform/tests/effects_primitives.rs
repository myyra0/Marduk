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
#[cfg(windows)]
use marduk_core::root_identity::UnsupportedReason;
use marduk_platform::effects::{
    CopyError, copy_file, detach, fingerprint_path, open_directory, publish, replace,
    sync_directory, volume_qualified,
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
    assert_eq!(
        error,
        CopyError::DestinationExists,
        "an existing destination is a conflict, not a metadata refusal"
    );
    assert_eq!(fs::read(&destination).unwrap(), STAGED);

    #[cfg(windows)]
    {
        // Creation and access times are copied for restore fidelity; the copy
        // is a new object with its own file id and change time.
        assert_eq!(
            creation_time(&destination),
            creation_time(&source),
            "the copy must carry the source's birth time"
        );
    }
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

/// A target that denies attribute writes must fail the exchange before either
/// name moves. The read handle still works, so a preview can admit the file;
/// only the timestamp authority is withheld.
#[cfg(windows)]
#[test]
fn replace_refuses_a_denied_attribute_write_before_moving_anything() {
    use std::os::windows::fs::OpenOptionsExt;
    use std::process::Command;
    use windows_sys::Win32::Storage::FileSystem::FILE_WRITE_ATTRIBUTES;

    let directory = tempfile::tempdir().unwrap();
    let stage_directory = directory.path().join("stage-directory");
    let target_directory = directory.path().join("target-directory");
    fs::create_dir(&stage_directory).unwrap();
    fs::create_dir(&target_directory).unwrap();
    let stage_path = stage_directory.join("stage");
    let target_path = target_directory.join("leaf");
    fs::write(&stage_path, STAGED).unwrap();
    fs::write(&target_path, ORIGINAL).unwrap();

    let acl = |arguments: &[&str]| {
        Command::new("icacls")
            .arg(&target_path)
            .args(arguments)
            .status()
            .is_ok_and(|status| status.success())
    };
    if !acl(&["/deny", "*S-1-1-0:(WA)"]) {
        eprintln!("skipped: this environment cannot deny FILE_WRITE_ATTRIBUTES");
        return;
    }
    if std::fs::OpenOptions::new()
        .access_mode(FILE_WRITE_ATTRIBUTES)
        .open(&target_path)
        .is_ok()
    {
        assert!(acl(&["/remove:d", "*S-1-1-0"]));
        eprintln!("skipped: an ordinary token still holds FILE_WRITE_ATTRIBUTES");
        return;
    }

    let stage = open_directory(&stage_directory).unwrap();
    let target = open_directory(&target_directory).unwrap();
    let result = replace(
        &stage,
        "stage",
        &target,
        "leaf",
        &stage_path,
        &target_path,
        &stage_path,
    );
    assert!(result.is_err(), "a denied attribute write must fail");
    assert_eq!(fs::read(&target_path).unwrap(), ORIGINAL);
    assert_eq!(fs::read(&stage_path).unwrap(), STAGED);
    assert!(!stage_directory.join("displaced").exists(), "no name moved");

    assert!(acl(&["/remove:d", "*S-1-1-0"]));
}

/// NTFS tunneling gives a rename into a just-vacated name the removed entry's
/// creation time; `replace` restores the staged birth time after its renames,
/// so identity must still match field for field.
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
    // NTFS tunneling gives a rename into a just-vacated name the removed
    // entry's creation time; `replace` restores the staged birth time after
    // its renames, so identity must still match field for field.
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
                "the record proves NTFS composition"
            ),
            Some(name) => {
                // exFAT/FAT32/ReFS volumes cannot be constructed portably here.
                eprintln!("the temporary directory is on {name}, not NTFS");
                assert!(!volume_qualified(directory.path()));
            }
            None => panic!("the temporary directory has no readable volume name"),
        }
    }
}

#[cfg(windows)]
#[test]
fn copy_refuses_concrete_file_write_data_in_source_dacl() {
    use windows_sys::Win32::Storage::FileSystem::{FILE_GENERIC_READ, FILE_GENERIC_WRITE};

    const DELETE: u32 = 0x0001_0000;
    refuse_widened_dacl(FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE);
}

#[cfg(windows)]
#[test]
fn copy_refuses_generic_write_in_source_dacl() {
    use windows_sys::Win32::Foundation::GENERIC_WRITE;
    use windows_sys::Win32::Storage::FileSystem::FILE_GENERIC_READ;

    refuse_widened_dacl(GENERIC_WRITE | FILE_GENERIC_READ);
}

#[cfg(windows)]
#[test]
fn copy_refuses_generic_all_in_source_dacl() {
    use windows_sys::Win32::Foundation::GENERIC_ALL;

    refuse_widened_dacl(GENERIC_ALL);
}

/// The source's DACL is replaced by one Everyone ACE with `mask`. Reading the
/// source must still work, so every mask below also carries read access.
#[cfg(windows)]
fn refuse_widened_dacl(mask: u32) {
    use std::mem::{offset_of, size_of};
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Security::Authorization::{SE_FILE_OBJECT, SetNamedSecurityInfoW};
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_REVISION, CreateWellKnownSid,
        DACL_SECURITY_INFORMATION, InitializeAcl, WinWorldSid,
    };

    const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;

    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    let destination = directory.path().join("destination");
    fs::write(&source, STAGED).unwrap();

    let mut sid = [0u8; 68];
    let mut sid_len = sid.len() as u32;
    assert_ne!(
        unsafe {
            CreateWellKnownSid(
                WinWorldSid,
                std::ptr::null_mut(),
                sid.as_mut_ptr().cast(),
                &mut sid_len,
            )
        },
        0,
        "could not build the Everyone SID"
    );
    let ace_size = offset_of!(ACCESS_ALLOWED_ACE, SidStart) + sid_len as usize;
    let acl_len = size_of::<ACL>() + ace_size;
    let mut acl = vec![0u8; acl_len];
    assert_ne!(
        unsafe { InitializeAcl(acl.as_mut_ptr().cast(), acl_len as u32, ACL_REVISION) },
        0
    );
    let ace = unsafe { acl.as_mut_ptr().add(size_of::<ACL>()) };
    unsafe {
        let header = ace.cast::<ACE_HEADER>();
        (*header).AceType = ACCESS_ALLOWED_ACE_TYPE;
        (*header).AceFlags = 0;
        (*header).AceSize = ace_size as u16;
        let allowed = ace.cast::<ACCESS_ALLOWED_ACE>();
        (*allowed).Mask = mask;
        std::ptr::copy_nonoverlapping(
            sid.as_ptr(),
            ace.add(offset_of!(ACCESS_ALLOWED_ACE, SidStart)),
            sid_len as usize,
        );
    }
    // `InitializeAcl` leaves AceCount at zero; an ACL with no ACEs grants
    // nothing and would make the guard a no-op. Record the one ACE we built.
    unsafe {
        (*acl.as_mut_ptr().cast::<ACL>()).AceCount = 1;
        (*acl.as_mut_ptr().cast::<ACL>()).AclSize = acl_len as u16;
    }
    let path: Vec<u16> = source.as_os_str().encode_wide().chain([0]).collect();
    assert_eq!(
        unsafe {
            SetNamedSecurityInfoW(
                path.as_ptr(),
                SE_FILE_OBJECT,
                windows_sys::Win32::Security::PROTECTED_DACL_SECURITY_INFORMATION
                    | windows_sys::Win32::Security::DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                acl.as_ptr().cast(),
                std::ptr::null_mut(),
            )
        },
        0,
        "could not widen the source DACL"
    );

    let mut owner: *mut std::ffi::c_void = std::ptr::null_mut();
    let mut group: *mut std::ffi::c_void = std::ptr::null_mut();
    let mut dacl: *mut windows_sys::Win32::Security::ACL = std::ptr::null_mut();
    let mut sacl: *mut windows_sys::Win32::Security::ACL = std::ptr::null_mut();
    let mut sd: *mut std::ffi::c_void = std::ptr::null_mut();
    let path: Vec<u16> = source.as_os_str().encode_wide().chain([0]).collect();
    assert_eq!(
        unsafe {
            windows_sys::Win32::Security::Authorization::GetNamedSecurityInfoW(
                path.as_ptr(),
                windows_sys::Win32::Security::Authorization::SE_FILE_OBJECT,
                windows_sys::Win32::Security::DACL_SECURITY_INFORMATION,
                &mut owner,
                &mut group,
                &mut dacl,
                &mut sacl,
                &mut sd,
            )
        },
        0,
        "could not read the source DACL back"
    );
    assert!(!dacl.is_null(), "the source DACL is absent");
    assert_eq!(
        unsafe { (*dacl).AceCount },
        1,
        "the source DACL must contain exactly one ACE"
    );
    let mut ace_ptr: *mut std::ffi::c_void = std::ptr::null_mut();
    assert_ne!(
        unsafe { windows_sys::Win32::Security::GetAce(dacl, 0, &mut ace_ptr) },
        0
    );
    let ace = unsafe { &*(ace_ptr.cast::<windows_sys::Win32::Security::ACCESS_ALLOWED_ACE>()) };
    // Windows expands generic rights when the ACE is stored, so compare the
    // effective grant, not the literal bit the witness passed in.
    const GENERIC_ALL: u32 = 0x1000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const MAXIMUM_ALLOWED: u32 = 0x0200_0000;
    const FILE_WRITE_DATA: u32 = 2;
    const FILE_APPEND_DATA: u32 = 4;
    const FILE_WRITE_EA: u32 = 16;
    const FILE_WRITE_ATTRIBUTES: u32 = 256;
    const FILE_DELETE_CHILD: u32 = 64;
    const WRITE_DAC: u32 = 0x0004_0000;
    const WRITE_OWNER: u32 = 0x0008_0000;
    const DELETE: u32 = 0x0001_0000;
    let write_rights = FILE_WRITE_DATA
        | FILE_APPEND_DATA
        | FILE_WRITE_EA
        | FILE_WRITE_ATTRIBUTES
        | FILE_DELETE_CHILD
        | WRITE_DAC
        | WRITE_OWNER
        | DELETE;
    let mut effective = ace.Mask;
    if effective & MAXIMUM_ALLOWED != 0 {
        effective = u32::MAX;
    }
    if effective & GENERIC_ALL != 0 {
        effective |= u32::MAX;
    }
    if effective & GENERIC_WRITE != 0 {
        effective |= windows_sys::Win32::Storage::FileSystem::FILE_GENERIC_WRITE;
    }
    assert_ne!(
        effective & write_rights,
        0,
        "the read-back ACE must carry the write bits under test"
    );

    let error =
        copy_file(&source, &destination).expect_err("a widened source DACL must be refused");
    assert_eq!(error, CopyError::Unsupported(UnsupportedReason::Metadata));
    assert!(
        !destination.exists(),
        "a refused copy must leave no artifact in the private effect area"
    );
    assert_eq!(fs::read(&source).unwrap(), STAGED);
}

#[cfg(windows)]
#[test]
fn staged_copy_preserves_owner_group_dacl_attributes_and_last_write() {
    use std::os::windows::fs::MetadataExt;
    use std::process::Command;

    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    const FILE_ATTRIBUTE_ARCHIVE: u32 = 0x20;

    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    let ordinary = directory.path().join("ordinary");
    let destination = directory.path().join("stage");
    fs::write(&source, STAGED).unwrap();
    fs::write(&ordinary, STAGED).unwrap();
    set_current_user_only_dacl(&source);
    assert!(
        Command::new("attrib")
            .arg("+H")
            .arg(&source)
            .status()
            .unwrap()
            .success(),
        "attrib +H must set a non-default attribute"
    );
    set_modified(&source, modified());

    let source_fingerprint = fingerprint_path(&source).unwrap();
    let ordinary_fingerprint = fingerprint_path(&ordinary).unwrap();
    assert_ne!(
        source_fingerprint.metadata, ordinary_fingerprint.metadata,
        "the staged copy must be observed against metadata that actually differs"
    );

    copy_file(&source, &destination).unwrap();
    let copy_fingerprint = fingerprint_path(&destination).unwrap();
    assert_eq!(
        copy_fingerprint.metadata, source_fingerprint.metadata,
        "the staged copy carries the source owner, group, DACL, attributes and last-write"
    );
    assert_eq!(fs::read(&destination).unwrap(), STAGED);
    let copied = fs::metadata(&destination).unwrap();
    assert_eq!(
        copied.file_attributes() & FILE_ATTRIBUTE_HIDDEN,
        FILE_ATTRIBUTE_HIDDEN
    );
    assert_ne!(copied.file_attributes() & FILE_ATTRIBUTE_ARCHIVE, 0);
    assert_eq!(
        copied.last_write_time(),
        fs::metadata(&source).unwrap().last_write_time()
    );
}

#[cfg(windows)]
#[test]
fn copy_succeeds_for_an_inherited_current_user_only_dacl() {
    let directory = tempfile::tempdir().unwrap();
    let parent = directory.path().join("parent");
    fs::create_dir(&parent).unwrap();
    set_current_user_only_dacl(&parent);

    let source = parent.join("source");
    fs::write(&source, STAGED).unwrap();
    let destination = directory.path().join("destination");

    let (len, _) = copy_file(&source, &destination)
        .expect("a source whose inherited DACL grants only the current user must copy");
    assert_eq!(len, STAGED.len() as u64);
    assert_eq!(fs::read(&destination).unwrap(), STAGED);
}

#[cfg(windows)]
fn set_current_user_only_dacl(path: &Path) {
    use std::mem::{offset_of, size_of};
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::Authorization::{SE_FILE_OBJECT, SetNamedSecurityInfoW};
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_REVISION, GetTokenInformation, InitializeAcl,
        TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::Storage::FileSystem::{FILE_GENERIC_READ, FILE_GENERIC_WRITE};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token: HANDLE = std::ptr::null_mut();
    assert_ne!(
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) },
        0
    );
    let mut needed = 0u32;
    unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed) };
    let mut buffer = vec![0u32; (needed as usize).div_ceil(size_of::<u32>())];
    assert_ne!(
        unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        },
        0
    );
    let sid = unsafe { (*(buffer.as_ptr().cast::<TOKEN_USER>())).User.Sid };
    let sid_len = unsafe { windows_sys::Win32::Security::GetLengthSid(sid) } as usize;
    unsafe { CloseHandle(token) };

    let ace_size = offset_of!(ACCESS_ALLOWED_ACE, SidStart) + sid_len;
    let acl_len = size_of::<ACL>() + ace_size;
    let mut acl = vec![0u8; acl_len];
    assert_ne!(
        unsafe { InitializeAcl(acl.as_mut_ptr().cast(), acl_len as u32, ACL_REVISION) },
        0
    );
    let ace = unsafe { acl.as_mut_ptr().add(size_of::<ACL>()) };
    unsafe {
        let header = ace.cast::<ACE_HEADER>();
        (*header).AceType = 0;
        (*header).AceFlags = 0;
        (*header).AceSize = ace_size as u16;
        let allowed = ace.cast::<ACCESS_ALLOWED_ACE>();
        (*allowed).Mask = FILE_GENERIC_READ | FILE_GENERIC_WRITE;
        std::ptr::copy_nonoverlapping(
            sid.cast::<u8>(),
            ace.add(offset_of!(ACCESS_ALLOWED_ACE, SidStart)),
            sid_len,
        );
    }
    unsafe {
        (*acl.as_mut_ptr().cast::<ACL>()).AceCount = 1;
        (*acl.as_mut_ptr().cast::<ACL>()).AclSize = acl_len as u16;
    }

    let path: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    assert_eq!(
        unsafe {
            SetNamedSecurityInfoW(
                path.as_ptr(),
                SE_FILE_OBJECT,
                windows_sys::Win32::Security::PROTECTED_DACL_SECURITY_INFORMATION
                    | windows_sys::Win32::Security::DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                acl.as_ptr().cast(),
                std::ptr::null_mut(),
            )
        },
        0,
        "could not set the current-user-only DACL"
    );
}

#[cfg(windows)]
#[test]
fn interrupted_replace_keeps_the_canonical_displaced_slot_and_resumes() {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};

    let directory = tempfile::tempdir().unwrap();
    let stage_directory = directory.path().join("stage-directory");
    let target_directory = directory.path().join("target-directory");
    fs::create_dir(&stage_directory).unwrap();
    fs::create_dir(&target_directory).unwrap();
    let stage_path = stage_directory.join("stage");
    let displaced_path = stage_directory.join("displaced");
    let target_path = target_directory.join("leaf");
    let stage = open_directory(&stage_directory).unwrap();
    let target = open_directory(&target_directory).unwrap();

    fs::write(&stage_path, STAGED).unwrap();
    set_modified(&stage_path, modified());
    fs::write(&target_path, ORIGINAL).unwrap();
    set_modified(&target_path, modified() - Duration::from_secs(60));
    let staged = fingerprint_path(&stage_path).unwrap();
    let original = fingerprint_path(&target_path).unwrap();

    // Deny delete sharing on the stage so rename 1 succeeds and rename 2 fails.
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(&stage_path)
        .unwrap();
    assert!(
        replace(
            &stage,
            "stage",
            &target,
            "leaf",
            &stage_path,
            &target_path,
            &stage_path,
        )
        .is_err(),
        "the held stage must interrupt the exchange after the first rename"
    );
    assert!(!target_path.exists());
    assert_eq!(fs::read(&stage_path).unwrap(), STAGED);
    assert_eq!(fs::read(&displaced_path).unwrap(), ORIGINAL);
    assert_eq!(
        fingerprint_path(&displaced_path).unwrap(),
        original,
        "the displaced bytes must be visible in the canonical retained slot"
    );
    sync_directory(&stage_directory).unwrap();
    drop(held);

    // The same call now resumes the interrupted exchange to its final shape.
    replace(
        &stage,
        "stage",
        &target,
        "leaf",
        &stage_path,
        &target_path,
        &stage_path,
    )
    .expect("a retry must complete the interrupted exchange");
    assert_eq!(fs::read(&target_path).unwrap(), STAGED);
    assert_eq!(fingerprint_path(&target_path).unwrap(), staged);
    assert_eq!(fs::read(&stage_path).unwrap(), ORIGINAL);
    assert_eq!(fingerprint_path(&stage_path).unwrap(), original);
    assert!(!displaced_path.exists());
}

#[cfg(windows)]
fn creation_time(path: &Path) -> i64 {
    use std::mem::size_of;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_BASIC_INFO, FileBasicInfo, GetFileInformationByHandleEx,
    };

    let file = File::open(path).unwrap();
    let mut basic = FILE_BASIC_INFO::default();
    assert_ne!(
        unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileBasicInfo,
                (&mut basic as *mut FILE_BASIC_INFO).cast(),
                size_of::<FILE_BASIC_INFO>() as u32,
            )
        },
        0
    );
    basic.CreationTime
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
