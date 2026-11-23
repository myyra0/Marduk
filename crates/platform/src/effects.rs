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
//!   stream, applies the source's attributes, creation/access/last-write times,
//!   owner, group and DACL, and calls `FlushFileBuffers` before returning.
//!   Creation and access times are copied for restore fidelity; the copy is
//!   still a new object, so its file id and change time differ. Extended
//!   attributes and alternate data streams are not copied. A source DACL that
//!   grants write-class access to any principal outside the current user and
//!   the SYSTEM/Administrators baseline is refused before the descriptor is
//!   applied, so the private effect area never inherits a write-class grant to
//!   another local user. Read and execute grants are copied verbatim; the
//!   private area's own DACL is what limits access to those bytes. A destination
//!   that appears first is refused, never overwritten, and every post-create
//!   failure removes the partial artifact.
//! - `publish` is one `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING`, so an
//!   existing target makes the call fail; the target bytes are untouched. The
//!   move is refused across volumes (`MOVEFILE_COPY_ALLOWED` is not set). NTFS
//!   tunneling would give the target name the creation time of the object it
//!   recently held, so the staged birth time is captured through a handle bound
//!   before the move, restored through that same handle afterwards, and re-read
//!   before success is reported. A denied attribute write therefore fails before
//!   the name changes.
//! - `replace` is an exchange of two no-replace renames in production: the
//!   target object moves to the canonical `<operation>/displaced` slot, then the
//!   staged object moves onto the target name. The target briefly has no name
//!   between those two moves, so this is not an atomic exchange. A caller that
//!   retains the displaced object at a different path performs a third no-replace
//!   rename from that slot. A cut after the first rename leaves the target
//!   absent, the staged bytes in the stage slot and the displaced bytes in the
//!   canonical slot; the next call resumes that shape. `ReplaceFileW` is
//!   deliberately not used: it merges the replaced file's creation time and
//!   DACL into the replacement, so the public target would no longer carry the
//!   sealed staged fingerprint. NTFS tunneling would also give each object the
//!   creation time of the name it lands on, so both birth times are captured
//!   through handles bound before the moves and restored through those same
//!   handles, then re-read, before success is reported. A denied attribute write
//!   therefore fails before any public name changes, including for a read-only
//!   original.
//!   Every failed step returns an error and the caller must re-observe; a failed
//!   exchange is never reported as a no-op.
//! - `detach` is one no-replace rename into the retain slot. Unknown bytes in
//!   the slot make the call fail rather than be replaced.
//! - `sync_directory` flushes a directory handle opened with
//!   `FILE_FLAG_BACKUP_SEMANTICS` via `FlushFileBuffers`. Windows documents no
//!   namespace persistence barrier for an ordinary token, so this is not
//!   claimed as power-cut durable evidence and the deployment barrier stays
//!   closed on Windows.
//!
//! None of these primitives prevents a same-privilege process from substituting
//! a parent or leaf between the caller's live-handle check and the native call.
//! Divergence is detected by post-effect reobservation and the displaced bytes
//! are retained; the accepted limitation and the rule that an error is not proof
//! of no change are owned by `docs/contracts/m1-recovery.md`.

use std::fs::File;
use std::io;

use marduk_core::root_identity::{Fingerprint, RootError, RootIdentity, UnsupportedReason};

/// Why [`copy_file`] could not produce a complete private copy.
///
/// This is deliberately narrower than [`RootError`]: an occupied destination is
/// a conflict for the caller's fresh inventory, while a transient lock is worth
/// retrying. Neither means the volume cannot preserve metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyError {
    /// The destination name already exists; nothing was overwritten.
    DestinationExists,
    /// The source or destination was transiently locked; a retry may succeed.
    Sharing,
    /// The requested metadata cannot be reproduced on this object.
    Unsupported(UnsupportedReason),
    /// The source or an ancestor is unavailable.
    TargetUnavailable,
    /// Another operating-system failure.
    Io,
}

impl std::fmt::Display for CopyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DestinationExists => formatter.write_str("the destination already exists"),
            Self::Sharing => formatter.write_str("the source or destination is locked"),
            Self::Unsupported(reason) => write!(formatter, "unsupported copy metadata: {reason}"),
            Self::TargetUnavailable => formatter.write_str("the copy source is unavailable"),
            Self::Io => formatter.write_str("a native copy failed"),
        }
    }
}

impl std::error::Error for CopyError {}

fn copy_root_error(error: RootError) -> CopyError {
    match error {
        RootError::TargetUnavailable => CopyError::TargetUnavailable,
        RootError::UnsupportedTarget(reason) => CopyError::Unsupported(reason),
        _ => CopyError::Io,
    }
}

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

/// Flush the directory that a public effect changed.
///
/// Linux fsyncs the pinned parent handle. Windows cannot flush the read-only
/// pinned handle, so it opens the parent pathname for write and calls
/// `FlushFileBuffers` ([ADR 0004](../../../docs/adr/0004-windows-writes-use-standard-flush.md)).
/// That is a standard flush, not a power-cut barrier.
#[cfg(target_os = "linux")]
pub fn sync_bound_directory(directory: &File, path: &std::path::Path) -> Result<(), RootError> {
    let _ = path;
    directory.sync_all().map_err(|_| RootError::Io)
}

#[cfg(windows)]
pub fn sync_bound_directory(directory: &File, path: &std::path::Path) -> Result<(), RootError> {
    let _ = directory;
    sync_directory(path)
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
        // A restore detaches the target name moments before the next deploy
        // publishes onto it. NTFS tunneling re-applies the detached entry's
        // creation time to the moved object, which would break the sealed
        // staged fingerprint. Bind the timestamp authority before the move so a
        // denied attribute write fails while both names are still untouched,
        // then restore the birth through that handle and never by name.
        let staged_file = timestamp_authority(stage_path).map_err(attribute_error)?;
        let staged_creation = creation_time(&staged_file).map_err(attribute_error)?;
        // No REPLACE_EXISTING and no COPY_ALLOWED: an existing target or another
        // volume fails the move and leaves both names as they were. The caller's
        // fresh observation classifies a reported failure as a conflict.
        move_path_noreplace(stage_path, target_path)?;
        restore_creation_time(&staged_file, staged_creation).map_err(attribute_error)
    }
}

/// Exchange `stage_dir/stage_name` with `target_dir/leaf`, so the target name
/// holds the staged bytes and the actually displaced file is retained at
/// `retain_path`. A missing target fails the exchange; bytes are never discarded.
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
        // replacement, which would break the sealed staged fingerprint. The
        // adapter's canonical retained slot is `<operation>/displaced`, so the
        // intermediate is derived from the stage path's file name rather than
        // appended to it; `evidence_for`, `prepare`'s leftover guard and
        // `sync_effect` all inspect that name after a cut.
        let displaced = if retain_path == stage_path {
            stage_path.with_file_name("displaced")
        } else {
            retain_path.to_path_buf()
        };
        // A cut between the renames leaves the target name absent with the
        // staged bytes in the stage slot and the displaced bytes in the
        // canonical intermediate. Resume that exact shape instead of demanding
        // a target that the interrupted exchange already moved away.
        let target_present = match std::fs::symlink_metadata(target_path) {
            Ok(_) => true,
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(_) => return Err(RootError::Io),
        };
        if !target_present {
            let resume = displaced.as_path() != stage_path
                && std::fs::symlink_metadata(stage_path).is_ok()
                && std::fs::symlink_metadata(&displaced).is_ok();
            if !resume {
                return Err(RootError::Io);
            }
            return complete_interrupted_replace(stage_path, target_path, &displaced, retain_path);
        }
        // NTFS tunneling re-applies the creation time of a name removed within
        // the last ~15 s to the next object added under it, including a rename.
        // Both renames below land on a name this exchange just vacated, so the
        // births are captured first and restored afterwards. The handles that
        // apply them are bound before the moves: a read-only or attribute-denying
        // original must fail before the game folder changes, not after.
        let displaced_file = timestamp_authority(target_path)?;
        let staged_file = timestamp_authority(stage_path)?;
        let staged_creation = creation_time(&staged_file)?;
        let displaced_creation = creation_time(&displaced_file)?;
        // 1. Retain the expected target under a name that must be absent.
        move_path_noreplace(target_path, &displaced).map_err(|_| RootError::Io)?;
        // 2. Publish the staged object with its identity and metadata intact.
        move_path_noreplace(stage_path, target_path).map_err(|_| RootError::Io)?;
        // 3. Put the displaced object in the caller's canonical retain slot.
        if displaced.as_path() != retain_path {
            move_path_noreplace(&displaced, retain_path).map_err(|_| RootError::Io)?;
        }
        restore_creation_time(&staged_file, staged_creation)?;
        restore_creation_time(&displaced_file, displaced_creation)?;
        Ok(())
    }
}

/// Complete a checked Windows exchange without ever moving an occupied target.
#[cfg(windows)]
pub fn complete_interrupted_replace(
    stage: &std::path::Path,
    target: &std::path::Path,
    displaced: &std::path::Path,
    retain: &std::path::Path,
) -> Result<(), RootError> {
    let displaced_file = timestamp_authority(displaced)?;
    let staged_file = timestamp_authority(stage)?;
    let staged_creation = creation_time(&staged_file)?;
    let displaced_creation = creation_time(&displaced_file)?;
    move_path_noreplace(stage, target).map_err(|_| RootError::Io)?;
    if displaced != retain {
        move_path_noreplace(displaced, retain).map_err(|_| RootError::Io)?;
    }
    restore_creation_time(&staged_file, staged_creation)?;
    restore_creation_time(&displaced_file, displaced_creation)
}

/// Revert a checked Windows exchange's first rename, leaving the stage alone.
#[cfg(windows)]
pub fn revert_interrupted_replace(
    displaced: &std::path::Path,
    target: &std::path::Path,
) -> Result<(), RootError> {
    let file = timestamp_authority(displaced)?;
    let creation = creation_time(&file)?;
    move_path_noreplace(displaced, target).map_err(|_| RootError::Io)?;
    restore_creation_time(&file, creation)
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

/// Whether the availability record proves the composed native effect adapter on
/// the volume holding `root`. `marduk_core::qualification` is the decision
/// owner; this function only observes the volume and asks. Deployment
/// composition refuses an unqualified installation or library root.
pub fn volume_qualified(root: &std::path::Path) -> bool {
    crate::native::effects_volume_qualified(root)
}

/// Copy `source` to a new file at `destination`, preserving the supported
/// metadata before the copy is reported. `create_new` fails instead of
/// overwriting an object that appeared at the destination. On Windows the data
/// flush is `FlushFileBuffers` on the file handle and is not a claim that the
/// directory entry survives a power cut.
#[cfg(target_os = "linux")]
pub fn copy_file(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> Result<(u64, [u8; 32]), CopyError> {
    use sha2::{Digest, Sha256};
    use std::fs::OpenOptions;
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;

    let mut input = crate::native::open_file(source).map_err(copy_root_error)?;
    let metadata = input.metadata().map_err(|_| CopyError::Io)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|error| match error.kind() {
            io::ErrorKind::AlreadyExists => CopyError::DestinationExists,
            io::ErrorKind::WouldBlock => CopyError::Sharing,
            _ => CopyError::Io,
        })?;
    let result = (|| {
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        let mut len = 0u64;
        loop {
            let read = input.read(&mut buffer).map_err(|_| CopyError::Io)?;
            if read == 0 {
                break;
            }
            output
                .write_all(&buffer[..read])
                .map_err(|_| CopyError::Io)?;
            hasher.update(&buffer[..read]);
            len += read as u64;
        }
        // Scoped metadata: mode, ownership and mtime. Eligibility already
        // refused links, xattrs, flags and special modes. Metadata lands before
        // the sync so the reported artifact is the complete copy.
        let mode = metadata.mode() & 0o7777;
        if unsafe { libc::fchmod(output.as_raw_fd(), mode as libc::mode_t) } != 0
            || unsafe { libc::fchown(output.as_raw_fd(), metadata.uid(), metadata.gid()) } != 0
        {
            return Err(CopyError::Unsupported(UnsupportedReason::Metadata));
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
            return Err(CopyError::Unsupported(UnsupportedReason::Metadata));
        }
        output.sync_all().map_err(|_| CopyError::Io)?;
        let mut sha256 = [0u8; 32];
        sha256.copy_from_slice(&hasher.finalize());
        Ok((len, sha256))
    })();
    match result {
        Ok(value) => Ok(value),
        Err(error) => {
            drop(output);
            let _ = std::fs::remove_file(destination);
            Err(error)
        }
    }
}

/// A complete copy: data, attributes, creation/access/last-write times and the
/// owner, group and DACL witness the caller holds for the source. Creation and
/// access times are copied so a restored file presents the source's timestamps;
/// the copy is still a new object, so its file id and change time differ.
/// Extended attributes and alternate data streams are not copied. The
/// destination is created with `CREATE_NEW`; an existing object is refused as
/// [`CopyError::DestinationExists`] and a transient lock as
/// [`CopyError::Sharing`]. A source DACL that grants write-class access to a
/// principal outside the current user and the SYSTEM/Administrators baseline is
/// refused before it can be applied to the private effect area. The DACL is
/// applied protected, so the copy does not also inherit ACEs from its parent.
/// Every post-create failure removes the partial
/// destination before returning. `FlushFileBuffers` covers this file handle, not
/// the directory entry.
#[cfg(windows)]
pub fn copy_file(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> Result<(u64, [u8; 32]), CopyError> {
    use sha2::{Digest, Sha256};
    use std::io::{Read, Write};
    use std::mem::size_of;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{
        ERROR_ALREADY_EXISTS, ERROR_FILE_EXISTS, ERROR_INSUFFICIENT_BUFFER, ERROR_LOCK_VIOLATION,
        ERROR_SHARING_VIOLATION, GENERIC_WRITE, GetLastError,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, GROUP_SECURITY_INFORMATION, GetKernelObjectSecurity,
        OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, SetKernelObjectSecurity,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_BASIC_INFO, FileBasicInfo, GetFileInformationByHandleEx,
        SetFileInformationByHandle, WRITE_DAC, WRITE_OWNER,
    };

    let unsupported = || CopyError::Unsupported(UnsupportedReason::Metadata);

    let mut input = open_file(source).map_err(copy_root_error)?;
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
        return Err(CopyError::Io);
    }
    if basic.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(CopyError::Unsupported(UnsupportedReason::ObjectKind));
    }

    // `SetKernelObjectSecurity` needs WRITE_DAC and WRITE_OWNER, which
    // `GENERIC_WRITE` does not include.
    let mut output = std::fs::OpenOptions::new()
        .access_mode(GENERIC_WRITE | WRITE_DAC | WRITE_OWNER)
        .create_new(true)
        .open(destination)
        .map_err(|error| match error.raw_os_error().map(|code| code as u32) {
            Some(ERROR_FILE_EXISTS | ERROR_ALREADY_EXISTS) => CopyError::DestinationExists,
            Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION) => CopyError::Sharing,
            _ => CopyError::Io,
        })?;

    let result = (|| {
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        let mut len = 0u64;
        loop {
            let read = input.read(&mut buffer).map_err(|_| CopyError::Io)?;
            if read == 0 {
                break;
            }
            output
                .write_all(&buffer[..read])
                .map_err(|_| CopyError::Io)?;
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
        {
            return Err(unsupported());
        }
        // Refuse to launder a widened source grant into the private effect
        // area. Owner and group are still copied; only the DACL is checked.
        if !source_dacl_grants_only_current_user(descriptor.as_mut_ptr().cast())? {
            return Err(unsupported());
        }
        // Apply the source DACL protected: without PROTECTED the destination
        // can also inherit ACEs from its parent, making the copy a superset.
        if unsafe {
            SetKernelObjectSecurity(
                output.as_raw_handle(),
                requested | PROTECTED_DACL_SECURITY_INFORMATION,
                descriptor.as_mut_ptr().cast(),
            )
        } == 0
        {
            return Err(unsupported());
        }

        // Times and attributes last, so nothing else can move the observed
        // values. Creation and access times are intentionally copied for
        // restore fidelity; the change time is left to the system.
        let stamp = FILE_BASIC_INFO {
            CreationTime: basic.CreationTime,
            LastAccessTime: basic.LastAccessTime,
            LastWriteTime: basic.LastWriteTime,
            ChangeTime: 0,
            FileAttributes: basic.FileAttributes,
        };
        if unsafe {
            SetFileInformationByHandle(
                output.as_raw_handle(),
                FileBasicInfo,
                (&stamp as *const FILE_BASIC_INFO).cast(),
                size_of::<FILE_BASIC_INFO>() as u32,
            )
        } == 0
        {
            return Err(unsupported());
        }
        output.sync_all().map_err(|_| CopyError::Io)?;

        let mut sha256 = [0u8; 32];
        sha256.copy_from_slice(&hasher.finalize());
        Ok((len, sha256))
    })();
    match result {
        Ok(value) => Ok(value),
        Err(error) => {
            drop(output);
            let _ = std::fs::remove_file(destination);
            Err(error)
        }
    }
}

/// Whether every write-granting allow ACE in the descriptor belongs to the
/// current user or the machine's privileged baseline (SYSTEM and
/// Administrators, which already control the private effect area). A null or
/// malformed DACL, an ACE class this code cannot parse, and an unavailable token
/// all fail closed.
#[cfg(windows)]
fn source_dacl_grants_only_current_user(
    descriptor: *mut std::ffi::c_void,
) -> Result<bool, CopyError> {
    use std::mem::{offset_of, size_of};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, CreateWellKnownSid, EqualSid, GetAce, GetLengthSid,
        GetSecurityDescriptorDacl, GetTokenInformation, INHERIT_ONLY_ACE, IsValidAcl, IsValidSid,
        SECURITY_MAX_SID_SIZE, TOKEN_QUERY, TOKEN_USER, TokenUser, WinBuiltinAdministratorsSid,
        WinLocalSystemSid,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_APPEND_DATA, FILE_DELETE_CHILD, FILE_GENERIC_WRITE, FILE_WRITE_ATTRIBUTES,
        FILE_WRITE_DATA, FILE_WRITE_EA, WRITE_DAC, WRITE_OWNER,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
    const ACCESS_DENIED_ACE_TYPE: u8 = 1;
    const SYSTEM_AUDIT_ACE_TYPE: u8 = 2;
    const SYSTEM_ALARM_ACE_TYPE: u8 = 3;
    const DELETE: u32 = 0x0001_0000;
    // Generic and maximum-rights bits. `STANDARD_RIGHTS_WRITE` aliases
    // `READ_CONTROL`, so it is expanded through `FILE_GENERIC_WRITE` rather than
    // tested alone; an ACE that grants only it grants no write access.
    const GENERIC_ALL: u32 = 0x1000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const MAXIMUM_ALLOWED: u32 = 0x0200_0000;

    let write_rights = FILE_WRITE_DATA
        | FILE_APPEND_DATA
        | FILE_WRITE_EA
        | FILE_WRITE_ATTRIBUTES
        | FILE_DELETE_CHILD
        | WRITE_DAC
        | WRITE_OWNER
        | DELETE;

    let mut token: HANDLE = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(CopyError::Io);
    }
    let mut needed = 0u32;
    unsafe {
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed);
    }
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
    unsafe {
        CloseHandle(token);
    }
    if !ok {
        return Err(CopyError::Io);
    }
    let current = unsafe { (*(buffer.as_ptr().cast::<TOKEN_USER>())).User.Sid };
    if current.is_null() || unsafe { IsValidSid(current) } == 0 {
        return Err(CopyError::Io);
    }
    let mut system_sid = [0u8; SECURITY_MAX_SID_SIZE as usize];
    let mut system_len = system_sid.len() as u32;
    let mut administrators_sid = [0u8; SECURITY_MAX_SID_SIZE as usize];
    let mut administrators_len = administrators_sid.len() as u32;
    if unsafe {
        CreateWellKnownSid(
            WinLocalSystemSid,
            std::ptr::null_mut(),
            system_sid.as_mut_ptr().cast(),
            &mut system_len,
        )
    } == 0
        || unsafe {
            CreateWellKnownSid(
                WinBuiltinAdministratorsSid,
                std::ptr::null_mut(),
                administrators_sid.as_mut_ptr().cast(),
                &mut administrators_len,
            )
        } == 0
    {
        return Err(CopyError::Io);
    }
    let trusted = |sid: *mut std::ffi::c_void| unsafe {
        EqualSid(sid, current) != 0
            || EqualSid(sid, system_sid.as_ptr().cast_mut().cast()) != 0
            || EqualSid(sid, administrators_sid.as_ptr().cast_mut().cast()) != 0
    };

    let mut present = 0;
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut defaulted = 0;
    if unsafe { GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted) }
        == 0
    {
        return Err(CopyError::Io);
    }
    // A null DACL grants every principal full control.
    if present == 0 {
        return Ok(false);
    }
    if dacl.is_null() || unsafe { IsValidAcl(dacl) } == 0 {
        return Err(CopyError::Io);
    }
    let count = unsafe { (*dacl).AceCount };
    for index in 0..count as u32 {
        let mut ace: *mut std::ffi::c_void = std::ptr::null_mut();
        if unsafe { GetAce(dacl, index, &mut ace) } == 0 {
            return Err(CopyError::Io);
        }
        let header = unsafe { &*(ace.cast::<ACE_HEADER>()) };
        match header.AceType {
            ACCESS_ALLOWED_ACE_TYPE => {
                if u32::from(header.AceFlags) & INHERIT_ONLY_ACE != 0 {
                    continue;
                }
                let allowed = unsafe { &*(ace.cast::<ACCESS_ALLOWED_ACE>()) };
                // Access checks expand generic rights before evaluation, so a
                // mask test on concrete bits alone misses `GENERIC_WRITE`.
                if allowed.Mask & MAXIMUM_ALLOWED != 0 {
                    return Ok(false);
                }
                let mut mask = allowed.Mask;
                if mask & GENERIC_ALL != 0 {
                    mask |= u32::MAX;
                }
                if mask & GENERIC_WRITE != 0 {
                    mask |= FILE_GENERIC_WRITE;
                }
                if mask & write_rights == 0 {
                    continue;
                }
                let sid = unsafe {
                    ace.cast::<u8>()
                        .add(offset_of!(ACCESS_ALLOWED_ACE, SidStart))
                        .cast::<std::ffi::c_void>()
                };
                if unsafe { GetLengthSid(sid) } == 0 {
                    return Err(CopyError::Io);
                }
                if !trusted(sid) {
                    return Ok(false);
                }
            }
            ACCESS_DENIED_ACE_TYPE | SYSTEM_AUDIT_ACE_TYPE | SYSTEM_ALARM_ACE_TYPE => {}
            // An allow class with a different layout may grant the same rights.
            _ => return Ok(false),
        }
    }
    Ok(true)
}

#[cfg(windows)]
fn wide(path: &std::path::Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;

    path.as_os_str().encode_wide().chain([0]).collect()
}

/// Represent a typed root refusal in the `io` channel `publish` reports on.
#[cfg(windows)]
fn attribute_error(error: RootError) -> io::Error {
    io::Error::other(error)
}

/// Open the timestamp-write authority for an object without following a final
/// link. Binding this handle before a rename keeps the authority across the
/// rename and makes a denial fail before any public name changes.
#[cfg(windows)]
fn timestamp_authority(path: &std::path::Path) -> Result<File, RootError> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_WRITE_ATTRIBUTES,
    };

    std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|_| RootError::Io)
}

/// Creation time of a bound object in FILETIME 100 ns units.
#[cfg(windows)]
fn creation_time(file: &File) -> Result<i64, RootError> {
    use std::mem::size_of;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_BASIC_INFO, FileBasicInfo, GetFileInformationByHandleEx,
    };

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

/// Rewrite a bound object's creation time and prove the write took effect.
#[cfg(windows)]
fn restore_creation_time(file: &File, expected: i64) -> Result<(), RootError> {
    use std::mem::size_of;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_BASIC_INFO, FileBasicInfo, GetFileInformationByHandleEx, SetFileTime,
    };

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
