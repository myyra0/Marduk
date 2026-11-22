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
//! Neither `publish`, `replace` nor `detach` reads or deletes an arbitrary
//! caller path: their destinations are operation-owned artifacts.

use std::fs::File;
use std::io;

use marduk_core::root_identity::{Fingerprint, RootError, RootIdentity};

/// Identity of the verified directory at `path`.
pub fn directory_identity(path: &std::path::Path) -> Result<RootIdentity, RootError> {
    crate::native::directory_identity(path)
}

/// Open a verified directory handle. The handle pins the directory; callers use
/// it for handle-relative effects.
pub fn open_directory(path: &std::path::Path) -> Result<File, RootError> {
    crate::native::open_directory(path)
}

/// Open an existing regular file without following a final link.
pub fn open_file(path: &std::path::Path) -> Result<File, RootError> {
    crate::native::open_file(path)
}

/// Flush a directory namespace change to stable storage. Windows has no
/// documented ordinary-token directory barrier and reports the configuration
/// unsupported.
pub fn sync_directory(path: &std::path::Path) -> Result<(), RootError> {
    crate::native::sync_directory(path)
}

/// Move `stage_dir/stage_name` onto `target_dir/leaf` only when the target name
/// is absent. An existing target is never replaced.
#[cfg_attr(not(windows), allow(unused_variables))]
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
        move_path(stage_path, target_path)
    }
}

/// Exchange `stage_dir/stage_name` with `target_dir/leaf`, so the target name
/// holds the staged bytes and the stage slot holds the actually displaced file.
/// A missing target fails the exchange; bytes are never discarded.
#[cfg_attr(not(windows), allow(unused_variables))]
pub fn replace(
    stage_dir: &File,
    stage_name: &str,
    target_dir: &File,
    leaf: &str,
    stage_path: &std::path::Path,
    target_path: &std::path::Path,
    retain_path: &std::path::Path,
) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        crate::native::renameat2(
            stage_dir,
            stage_name,
            target_dir,
            leaf,
            libc::RENAME_EXCHANGE,
        )
    }
    #[cfg(windows)]
    {
        replace_file(target_path, stage_path, retain_path)
    }
}

/// Move the existing target at `target_dir/leaf` into `displaced_dir` without
/// replacing anything there. The target's bytes are preserved, not deleted.
#[cfg_attr(not(windows), allow(unused_variables))]
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
        move_path(target_path, displaced_path)
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
    crate::native::library_volume_qualified(root)
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

/// A scoped copy that restores attributes, timestamps and security is not
/// runtime-qualified on NTFS, and a data-only copy silently drops them. Fail
/// preparation closed instead of faking a complete backup.
#[cfg(windows)]
pub fn copy_file(
    _source: &std::path::Path,
    _destination: &std::path::Path,
) -> Result<(u64, [u8; 32]), RootError> {
    Err(RootError::UnsupportedTarget(
        marduk_core::root_identity::UnsupportedReason::Configuration,
    ))
}

#[cfg(windows)]
fn wide(path: &std::path::Path) -> io::Result<Vec<u16>> {
    path.to_str()
        .map(|text| text.encode_utf16().chain([0]).collect())
        .ok_or_else(|| io::Error::from_raw_os_error(87))
}

#[cfg(windows)]
fn move_path(from: &std::path::Path, to: &std::path::Path) -> io::Result<()> {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;

    let from = wide(from)?;
    let to = wide(to)?;
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0) } != 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(
            unsafe { GetLastError() } as i32
        ))
    }
}

#[cfg(windows)]
fn replace_file(
    target: &std::path::Path,
    replacement: &std::path::Path,
    backup: &std::path::Path,
) -> io::Result<()> {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;

    let target = wide(target)?;
    let replacement = wide(replacement)?;
    let backup = wide(backup)?;
    if unsafe {
        ReplaceFileW(
            target.as_ptr(),
            replacement.as_ptr(),
            backup.as_ptr(),
            0,
            std::ptr::null(),
            std::ptr::null(),
        )
    } != 0
    {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(
            unsafe { GetLastError() } as i32
        ))
    }
}
