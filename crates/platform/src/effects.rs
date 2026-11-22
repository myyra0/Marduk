//! Native effect primitives for the checked copy adapter.
//!
//! These are the operating-system candidates the deployment state machine
//! drives: publish into an absent name without clobbering, exchange an existing
//! file while retaining the actually displaced object, detach without deleting
//! unknown bytes, and copy a file with its supported metadata. The module owns
//! no journal, plan or retry policy; callers still re-verify the live root
//! binding before every effect and classify the result from fresh evidence.
//!
//! Diagnostic path arguments are used only where the Win32 API needs a
//! pathname. The Linux candidates act on directory handles and relative names.
//! `publish`, `replace` and `detach` move files between a caller-named target
//! and operation-owned stage or retain slots; they never read or delete an
//! arbitrary caller path.
//!
//! ## Windows guarantees
//!
//! - `copy_file` creates the destination with `CREATE_NEW`, copies the data
//!   stream, applies the source's attributes, last-write time, owner, group and
//!   DACL, and calls `FlushFileBuffers` before returning. Birth time, extended
//!   attributes and alternate data streams are not copied; the caller already
//!   refused such a source, and a copy is a new object. A destination that
//!   appears first is refused, never overwritten.
//! - `publish` is one `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING`, so an
//!   existing target makes the call fail; the target bytes are untouched. The
//!   move is refused across volumes (`MOVEFILE_COPY_ALLOWED` is not set).
//! - `replace` is an exchange of three no-replace renames: the target object
//!   moves to the retain slot, then the staged object moves onto the target
//!   name. The target briefly has no name between those two moves, so this is
//!   not an atomic exchange. `ReplaceFileW` is deliberately not used: it merges
//!   the replaced file's creation time and DACL into the replacement, so the
//!   public target would no longer carry the sealed staged fingerprint. NTFS
//!   tunneling would also give each object the creation time of the name it
//!   lands on, so both birth times are captured before the moves and restored,
//!   then re-read, before success is reported. Every failed step returns an
//!   error and the caller must re-observe; a failed exchange is never reported
//!   as a no-op.
//! - `detach` is one no-replace rename into the retain slot. Unknown bytes in
//!   the slot make the call fail rather than be replaced.
//! - `sync_directory` flushes a directory handle opened with
//!   `FILE_FLAG_BACKUP_SEMANTICS` via `FlushFileBuffers`. Windows documents no
//!   namespace persistence barrier for an ordinary token, so this is not
//!   claimed as power-cut durable evidence and the deployment barrier stays
//!   closed on Windows.
//!
//! None of these primitives prevents a same-privilege process from substituting
//! a parent or leaf between the caller's live-handle check and the native call;
//! see the accepted limitation in `docs/contracts/m1-recovery.md`. Divergence is
//! detected by post-effect reobservation and the displaced bytes are retained.

use std::fs::File;
use std::io;

use marduk_core::root_identity::{Fingerprint, RootError, RootIdentity};

/// Identity of the verified directory at `path`.
pub fn directory_identity(path: &std::path::Path) -> Result<RootIdentity, RootError> {
    crate::native::directory_identity(path)
}

/// Open a verified directory handle. The handle pins the directory; callers use
/// it for handle-relative effects.
#[cfg(target_os = "linux")]
pub fn open_directory(path: &std::path::Path) -> Result<File, RootError> {
    crate::native::open_directory(path)
}

/// Open a directory handle for the Win32 effect paths. A directory needs backup
/// semantics to be opened as a handle.
#[cfg(windows)]
pub fn open_directory(path: &std::path::Path) -> Result<File, RootError> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;

    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .map_err(|_| RootError::TargetUnavailable)
}

/// Open an existing regular file without following a final link.
#[cfg(target_os = "linux")]
pub fn open_file(path: &std::path::Path) -> Result<File, RootError> {
    crate::native::open_file(path)
}

#[cfg(windows)]
pub fn open_file(path: &std::path::Path) -> Result<File, RootError> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|_| RootError::TargetUnavailable)
}

/// Flush a directory namespace change to stable storage.
#[cfg(target_os = "linux")]
pub fn sync_directory(path: &std::path::Path) -> Result<(), RootError> {
    crate::native::sync_directory(path)
}

/// Flush a directory handle so its entries reach stable storage.
///
/// Win32 has no documented ordinary-token namespace barrier: this reports the
/// result of `FlushFileBuffers` on a directory handle and nothing more. Callers
/// must not treat it as proof that an entry survives an unexpected power loss.
#[cfg(windows)]
pub fn sync_directory(path: &std::path::Path) -> Result<(), RootError> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;

    // `FlushFileBuffers` requires write access to the handle.
    let directory = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .map_err(|_| RootError::TargetUnavailable)?;
    directory.sync_all().map_err(|_| RootError::Io)
}

/// Move `stage_dir/stage_name` onto `target_dir/leaf` only when the target name
/// is absent. An existing target is never replaced.
#[allow(unused_variables)]
pub fn publish(
    stage_dir: &File,
    stage_name: &str,
    target_dir: &File,
    leaf: &str,
    stage_path: &std::path::Path,
    target_path: &std::path::Path,
) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        crate::native::renameat2(
            stage_dir,
            stage_name,
            target_dir,
            leaf,
            libc::RENAME_NOREPLACE,
        )
    }
    #[cfg(windows)]
    {
        // No REPLACE_EXISTING and no COPY_ALLOWED: an existing target or another
        // volume fails the move and leaves both names as they were. The caller's
        // fresh observation classifies a reported failure as a conflict.
        move_path_noreplace(stage_path, target_path)
    }
}

/// Exchange `stage_dir/stage_name` with `target_dir/leaf`, so the target name
/// holds the staged bytes and the stage slot holds the actually displaced file.
/// A missing target fails the exchange; bytes are never discarded.
#[allow(unused_variables)]
pub fn replace(
    stage_dir: &File,
    stage_name: &str,
    target_dir: &File,
    leaf: &str,
    stage_path: &std::path::Path,
    target_path: &std::path::Path,
    retain_path: &std::path::Path,
) -> Result<(), RootError> {
    #[cfg(target_os = "linux")]
    {
        crate::native::renameat2(
            stage_dir,
            stage_name,
            target_dir,
            leaf,
            libc::RENAME_EXCHANGE,
        )
        .map_err(|_| RootError::Io)
    }
    #[cfg(windows)]
    {
        // The callers name the stage slot as the retain alias, so the exchange
        // needs one distinct intermediate name. `ReplaceFileW` cannot be used:
        // it merges the replaced file's creation time and DACL into the
        // replacement, which would break the sealed staged fingerprint. A
        // deterministic operation-owned name keeps the displaced bytes visible
        // when the exchange is cut between its moves.
        let displaced = if retain_path == stage_path {
            let mut name = std::ffi::OsString::from(stage_name);
            name.push(".displaced");
            stage_path.with_file_name(name)
        } else {
            retain_path.to_path_buf()
        };
        // NTFS tunneling re-applies the creation time of a name removed within
        // the last ~15 s to the next object added under it, including a rename.
        // Both renames below land on a name this exchange just vacated, so the
        // births are captured first and restored afterwards.
        let staged_creation = creation_time(stage_path)?;
        let displaced_creation = creation_time(target_path)?;
        // 1. Retain the expected target under a name that must be absent.
        move_path_noreplace(target_path, &displaced).map_err(|_| RootError::Io)?;
        // 2. Publish the staged object with its identity and metadata intact.
        move_path_noreplace(stage_path, target_path).map_err(|_| RootError::Io)?;
        // 3. Put the displaced object in the caller's canonical retain slot.
        if displaced.as_path() != retain_path {
            move_path_noreplace(&displaced, retain_path).map_err(|_| RootError::Io)?;
        }
        restore_creation_time(target_path, staged_creation)?;
        restore_creation_time(retain_path, displaced_creation)?;
        Ok(())
    }
}

/// Move the existing target at `target_dir/leaf` into `displaced_dir` without
/// replacing anything there. The target's bytes are preserved, not deleted.
#[allow(unused_variables)]
pub fn detach(
    target_dir: &File,
    leaf: &str,
    displaced_dir: &File,
    displaced_name: &str,
    target_path: &std::path::Path,
    displaced_path: &std::path::Path,
) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        crate::native::renameat2(
            target_dir,
            leaf,
            displaced_dir,
            displaced_name,
            libc::RENAME_NOREPLACE,
        )
    }
    #[cfg(windows)]
    {
        move_path_noreplace(target_path, displaced_path)
    }
}

/// Fingerprint an object without following a final link.
pub fn fingerprint_path(path: &std::path::Path) -> Result<Fingerprint, RootError> {
    #[cfg(target_os = "linux")]
    {
        let file = crate::native::open_file(path)?;
        crate::native::fingerprint_open_file(&file)
    }
    #[cfg(windows)]
    {
        crate::native::fingerprint_path(path)
    }
}

/// Whether the adapter has recorded runtime evidence for native effect writes on
/// the volume holding `root`. Deployment composition refuses an unqualified
/// installation or library root.
pub fn volume_qualified(root: &std::path::Path) -> bool {
    crate::native::effects_volume_qualified(root)
}

/// Copy `source` to a new file at `destination`, preserving the supported
/// metadata before the durable copy is reported. `create_new` fails instead of
/// overwriting an object that appeared at the destination.
#[cfg(target_os = "linux")]
pub fn copy_file(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> Result<(u64, [u8; 32]), RootError> {
    use sha2::{Digest, Sha256};
    use std::fs::OpenOptions;
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;

    let mut input = crate::native::open_file(source)?;
    let metadata = input.metadata().map_err(|_| RootError::Io)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|_| {
            RootError::UnsupportedTarget(marduk_core::root_identity::UnsupportedReason::Metadata)
        })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut len = 0u64;
    loop {
        let read = input.read(&mut buffer).map_err(|_| RootError::Io)?;
        if read == 0 {
            break;
        }
        output
            .write_all(&buffer[..read])
            .map_err(|_| RootError::Io)?;
        hasher.update(&buffer[..read]);
        len += read as u64;
    }
    // Scoped metadata: mode, ownership and mtime. Eligibility already refused
    // links, xattrs, flags and special modes. Metadata lands before the sync so
    // the durable artifact is the complete copy.
    let mode = metadata.mode() & 0o7777;
    if unsafe { libc::fchmod(output.as_raw_fd(), mode as libc::mode_t) } != 0
        || unsafe { libc::fchown(output.as_raw_fd(), metadata.uid(), metadata.gid()) } != 0
    {
        return Err(RootError::UnsupportedTarget(
            marduk_core::root_identity::UnsupportedReason::Metadata,
        ));
    }
    let times = [
        libc::timespec {
            tv_sec: metadata.atime(),
            tv_nsec: metadata.atime_nsec(),
        },
        libc::timespec {
            tv_sec: metadata.mtime(),
            tv_nsec: metadata.mtime_nsec(),
        },
    ];
    if unsafe { libc::futimens(output.as_raw_fd(), times.as_ptr()) } != 0 {
        return Err(RootError::UnsupportedTarget(
            marduk_core::root_identity::UnsupportedReason::Metadata,
        ));
    }
    output.sync_all().map_err(|_| RootError::Io)?;
    let mut sha256 = [0u8; 32];
    sha256.copy_from_slice(&hasher.finalize());
    Ok((len, sha256))
}

/// A complete copy: data, attributes, last-write time and the owner, group and
/// DACL witness the caller holds for the source. Birth time, extended
/// attributes and alternate data streams are not copied. The destination is
/// created with `CREATE_NEW`; an existing object is refused. If the supported
/// metadata cannot be reproduced the preparation fails closed with
/// `UnsupportedTarget(Metadata)` instead of yielding a partial backup.
/// `FlushFileBuffers` covers this file handle, not the directory entry.
#[cfg(windows)]
pub fn copy_file(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> Result<(u64, [u8; 32]), RootError> {
    use sha2::{Digest, Sha256};
    use std::io::{Read, Write};
    use std::mem::size_of;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, GENERIC_WRITE, GetLastError};
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, GROUP_SECURITY_INFORMATION, GetKernelObjectSecurity,
        OWNER_SECURITY_INFORMATION, SetKernelObjectSecurity,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_BASIC_INFO, FileBasicInfo, GetFileInformationByHandleEx,
        SetFileInformationByHandle, WRITE_DAC, WRITE_OWNER,
    };

    let unsupported =
        || RootError::UnsupportedTarget(marduk_core::root_identity::UnsupportedReason::Metadata);

    let mut input = open_file(source)?;
    let mut basic = FILE_BASIC_INFO::default();
    if unsafe {
        GetFileInformationByHandleEx(
            input.as_raw_handle(),
            FileBasicInfo,
            (&mut basic as *mut FILE_BASIC_INFO).cast(),
            size_of::<FILE_BASIC_INFO>() as u32,
        )
    } == 0
    {
        return Err(RootError::Io);
    }
    if basic.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(RootError::UnsupportedTarget(
            marduk_core::root_identity::UnsupportedReason::ObjectKind,
        ));
    }

    // `SetKernelObjectSecurity` needs WRITE_DAC and WRITE_OWNER, which
    // `GENERIC_WRITE` does not include.
    let mut output = std::fs::OpenOptions::new()
        .access_mode(GENERIC_WRITE | WRITE_DAC | WRITE_OWNER)
        .create_new(true)
        .open(destination)
        .map_err(|_| unsupported())?;

    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut len = 0u64;
    loop {
        let read = input.read(&mut buffer).map_err(|_| RootError::Io)?;
        if read == 0 {
            break;
        }
        output
            .write_all(&buffer[..read])
            .map_err(|_| RootError::Io)?;
        hasher.update(&buffer[..read]);
        len += read as u64;
    }

    // The observed security witness is owner, group and DACL.
    let requested =
        OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
    let mut needed = 0u32;
    if unsafe {
        GetKernelObjectSecurity(
            input.as_raw_handle(),
            requested,
            std::ptr::null_mut(),
            0,
            &mut needed,
        )
    } == 0
        && unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER
    {
        return Err(unsupported());
    }
    if needed == 0 {
        return Err(unsupported());
    }
    // A security descriptor must be four-byte aligned; `Vec<u32>` supplies that.
    let mut descriptor = vec![0u32; (needed as usize).div_ceil(size_of::<u32>())];
    let capacity = needed;
    if unsafe {
        GetKernelObjectSecurity(
            input.as_raw_handle(),
            requested,
            descriptor.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
        || needed > capacity
        || unsafe {
            SetKernelObjectSecurity(
                output.as_raw_handle(),
                requested,
                descriptor.as_mut_ptr().cast(),
            )
        } == 0
    {
        return Err(unsupported());
    }

    // Attributes and last-write time last, so nothing else can move the
    // observed values. Creation and access times are left at the new object's
    // own values; the copy is not the source object.
    let basic = FILE_BASIC_INFO {
        LastWriteTime: basic.LastWriteTime,
        FileAttributes: basic.FileAttributes,
        ..basic
    };
    if unsafe {
        SetFileInformationByHandle(
            output.as_raw_handle(),
            FileBasicInfo,
            (&basic as *const FILE_BASIC_INFO).cast(),
            size_of::<FILE_BASIC_INFO>() as u32,
        )
    } == 0
    {
        return Err(unsupported());
    }
    output.sync_all().map_err(|_| RootError::Io)?;

    let mut sha256 = [0u8; 32];
    sha256.copy_from_slice(&hasher.finalize());
    Ok((len, sha256))
}

#[cfg(windows)]
fn wide(path: &std::path::Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;

    path.as_os_str().encode_wide().chain([0]).collect()
}

/// Creation time of an existing object in FILETIME 100 ns units.
#[cfg(windows)]
fn creation_time(path: &std::path::Path) -> Result<i64, RootError> {
    use std::mem::size_of;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_BASIC_INFO, FileBasicInfo, GetFileInformationByHandleEx,
    };

    let file = open_file(path)?;
    let mut basic = FILE_BASIC_INFO::default();
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileBasicInfo,
            (&mut basic as *mut FILE_BASIC_INFO).cast(),
            size_of::<FILE_BASIC_INFO>() as u32,
        )
    } == 0
    {
        return Err(RootError::Io);
    }
    Ok(basic.CreationTime)
}

/// Rewrite an object's creation time and prove the write took effect.
#[cfg(windows)]
fn restore_creation_time(path: &std::path::Path, expected: i64) -> Result<(), RootError> {
    use std::mem::size_of;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_BASIC_INFO, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_WRITE_ATTRIBUTES,
        FileBasicInfo, GetFileInformationByHandleEx, SetFileTime,
    };

    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|_| RootError::Io)?;
    // Null access/write pointers leave those times unchanged.
    let creation = FILETIME {
        dwLowDateTime: expected as u32,
        dwHighDateTime: (expected >> 32) as u32,
    };
    if unsafe {
        SetFileTime(
            file.as_raw_handle(),
            &creation,
            std::ptr::null(),
            std::ptr::null(),
        )
    } == 0
    {
        return Err(RootError::Io);
    }
    let mut observed = FILE_BASIC_INFO::default();
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileBasicInfo,
            (&mut observed as *mut FILE_BASIC_INFO).cast(),
            size_of::<FILE_BASIC_INFO>() as u32,
        )
    } == 0
        || observed.CreationTime != expected
    {
        return Err(RootError::Io);
    }
    Ok(())
}

/// Move a single name without replacing an existing destination. No
/// `MOVEFILE_COPY_ALLOWED`: a cross-volume move is refused rather than turned
/// into a copy that would lose the staged identity.
#[cfg(windows)]
fn move_path_noreplace(from: &std::path::Path, to: &std::path::Path) -> io::Result<()> {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;

    let from = wide(from);
    let to = wide(to);
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0) } != 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(
            unsafe { GetLastError() } as i32
        ))
    }
}
