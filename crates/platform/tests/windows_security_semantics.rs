//! Windows effect-copy security witnesses.
//!
//! The staged copy must carry exactly the source DACL, applied protected so the
//! copy cannot also inherit ACEs from the private effect directory. A source
//! that already inherits a write grant for an untrusted principal stays refused.

#![cfg(windows)]

use std::fs::{self, File};
use std::mem::{offset_of, size_of};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;

use marduk_core::root_identity::UnsupportedReason;
use marduk_platform::effects::{CopyError, copy_file, fingerprint_path};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_INSUFFICIENT_BUFFER, GetLastError, HANDLE,
};
use windows_sys::Win32::Security::Authorization::{SE_FILE_OBJECT, SetNamedSecurityInfoW};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_REVISION, ACL_SIZE_INFORMATION, AclSizeInformation,
    AddAccessAllowedAceEx, CONTAINER_INHERIT_ACE, CreateWellKnownSid, DACL_SECURITY_INFORMATION,
    EqualSid, GetAce, GetAclInformation, GetKernelObjectSecurity, GetLengthSid,
    GetSecurityDescriptorDacl, GetTokenInformation, INHERIT_ONLY_ACE, INHERITED_ACE, InitializeAcl,
    IsValidAcl, IsValidSid, OBJECT_INHERIT_ACE, PROTECTED_DACL_SECURITY_INFORMATION, TOKEN_QUERY,
    TOKEN_USER, TokenUser, WinWorldSid,
};
use windows_sys::Win32::Storage::FileSystem::{FILE_GENERIC_READ, FILE_GENERIC_WRITE};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

const STAGED: &[u8] = b"staged bytes\n";

fn current_user_sid() -> Vec<u32> {
    let mut token: HANDLE = std::ptr::null_mut();
    assert_ne!(
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) },
        0
    );
    let mut needed = 0u32;
    unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed) };
    let mut buffer = vec![0u32; (needed as usize).div_ceil(size_of::<u32>())];
    let ok = needed != 0
        && unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        } != 0;
    unsafe { CloseHandle(token) };
    assert!(ok, "the token user SID must be readable");
    let sid = unsafe { (*(buffer.as_ptr().cast::<TOKEN_USER>())).User.Sid };
    let length = unsafe { GetLengthSid(sid) } as usize;
    assert_ne!(length, 0);
    let mut aligned = vec![0u32; length.div_ceil(size_of::<u32>())];
    unsafe {
        std::ptr::copy_nonoverlapping(sid.cast::<u8>(), aligned.as_mut_ptr().cast(), length);
    }
    aligned
}

fn world_sid() -> Vec<u32> {
    let mut sid = vec![0u8; 68]; // SECURITY_MAX_SID_SIZE
    let mut length = sid.len() as u32;
    assert_ne!(
        unsafe {
            CreateWellKnownSid(
                WinWorldSid,
                std::ptr::null_mut(),
                sid.as_mut_ptr().cast(),
                &mut length,
            )
        },
        0
    );
    let words = (length as usize).div_ceil(size_of::<u32>());
    let mut aligned = vec![0u32; words];
    unsafe {
        std::ptr::copy_nonoverlapping(sid.as_ptr(), aligned.as_mut_ptr().cast(), length as usize);
    }
    aligned
}

/// Apply one protected DACL to `path`; `entries` are `(ace_flags, mask, sid)`.
fn apply_protected_dacl(path: &Path, entries: &[(u32, u32, &[u32])]) {
    let bytes = size_of::<ACL>()
        + entries
            .iter()
            .map(|(_, _, sid)| 8 + sid.len() * size_of::<u32>())
            .sum::<usize>();
    let mut words = vec![0u32; bytes.div_ceil(size_of::<u32>())];
    let acl: *mut ACL = words.as_mut_ptr().cast();
    assert_ne!(unsafe { InitializeAcl(acl, bytes as u32, ACL_REVISION) }, 0);
    for (flags, mask, sid) in entries {
        assert_ne!(
            unsafe {
                AddAccessAllowedAceEx(
                    acl,
                    ACL_REVISION,
                    *flags,
                    *mask,
                    sid.as_ptr().cast_mut().cast(),
                )
            },
            0
        );
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    assert_eq!(
        unsafe {
            SetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                acl,
                std::ptr::null_mut(),
            )
        },
        0,
        "the fixture DACL must be applied"
    );
}

#[test]
fn protected_copy_descriptor_equals_source() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    fs::write(&source, STAGED).unwrap();
    let user = current_user_sid();
    apply_protected_dacl(
        &source,
        &[(0, FILE_GENERIC_READ | FILE_GENERIC_WRITE, &user)],
    );

    let source_fingerprint = fingerprint_path(&source).unwrap();
    let destination = directory.path().join("stage");
    copy_file(&source, &destination).unwrap();
    let copy_fingerprint = fingerprint_path(&destination).unwrap();

    assert_eq!(
        copy_fingerprint.metadata, source_fingerprint.metadata,
        "the protected copy must carry the source owner, group and DACL"
    );
}

#[test]
fn inherited_write_ace_on_source_is_refused() {
    let directory = tempfile::tempdir().unwrap();
    let parent = directory.path().join("parent");
    fs::create_dir(&parent).unwrap();
    let world = world_sid();
    apply_protected_dacl(
        &parent,
        &[(
            OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE,
            FILE_GENERIC_READ | FILE_GENERIC_WRITE,
            &world,
        )],
    );

    let source = parent.join("source");
    fs::write(&source, STAGED).unwrap();
    assert!(
        inherits_world_write(&source, &world),
        "the fixture source must inherit the parent's write grant"
    );

    let destination = directory.path().join("stage");
    let error = copy_file(&source, &destination)
        .expect_err("an inherited write grant for an untrusted principal must be refused");
    assert_eq!(error, CopyError::Unsupported(UnsupportedReason::Metadata));
    assert!(
        !destination.exists(),
        "a refused copy must leave no artifact in the private effect area"
    );
}

/// Witness-only read of the source DACL, independent of the copy path.
fn inherits_world_write(path: &Path, world: &[u32]) -> bool {
    let file = File::open(path).unwrap();
    let mut needed = 0u32;
    if unsafe {
        GetKernelObjectSecurity(
            file.as_raw_handle(),
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            0,
            &mut needed,
        )
    } == 0
        && unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER
    {
        return false;
    }
    let mut descriptor = vec![0u32; (needed as usize).div_ceil(size_of::<u32>())];
    let capacity = needed;
    if unsafe {
        GetKernelObjectSecurity(
            file.as_raw_handle(),
            DACL_SECURITY_INFORMATION,
            descriptor.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
        || needed > capacity
    {
        return false;
    }
    let base = descriptor.as_ptr().cast_mut().cast();
    let mut present = 0;
    let mut acl: *mut ACL = std::ptr::null_mut();
    let mut defaulted = 0;
    if unsafe { GetSecurityDescriptorDacl(base, &mut present, &mut acl, &mut defaulted) } == 0
        || present == 0
        || acl.is_null()
        || unsafe { IsValidAcl(acl) } == 0
    {
        return false;
    }
    let mut information = ACL_SIZE_INFORMATION::default();
    if unsafe {
        GetAclInformation(
            acl,
            (&mut information as *mut ACL_SIZE_INFORMATION).cast(),
            size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    } == 0
    {
        return false;
    }
    let world: *mut std::ffi::c_void = world.as_ptr().cast_mut().cast();
    for index in 0..information.AceCount {
        let mut ace: *mut std::ffi::c_void = std::ptr::null_mut();
        if unsafe { GetAce(acl, u32::from(index), &mut ace) } == 0 || ace.is_null() {
            return false;
        }
        let header = unsafe { std::ptr::read(ace.cast::<ACE_HEADER>()) };
        if header.AceType != 0
            || u32::from(header.AceFlags) & INHERITED_ACE == 0
            || u32::from(header.AceFlags) & INHERIT_ONLY_ACE != 0
        {
            continue;
        }
        let allowed = unsafe { std::ptr::read(ace.cast::<ACCESS_ALLOWED_ACE>()) };
        let entry: *mut std::ffi::c_void = unsafe {
            ace.cast::<u8>()
                .add(offset_of!(ACCESS_ALLOWED_ACE, SidStart))
                .cast()
        };
        if unsafe { IsValidSid(entry) } != 0
            && unsafe { EqualSid(entry, world) } != 0
            && allowed.Mask & FILE_GENERIC_WRITE != 0
        {
            return true;
        }
    }
    false
}
