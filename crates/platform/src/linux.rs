//! Linux writer gate: an exclusive `flock` on a stable, no-follow regular file.

use std::ffi::CString;
use std::fs::File;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::GateError;

/// Stable lock object for one application-owned root.
#[derive(Debug)]
pub struct WriterGate {
    path: PathBuf,
    device: u64,
    inode: u64,
}

/// Held exclusive lock. Closing the file description releases the lock.
#[derive(Debug)]
pub struct WriterGuard {
    _file: File,
}

impl WriterGate {
    /// Open or create the lock file and remember its identity. The parent
    /// directory must already exist and be owned by the application.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, GateError> {
        let path = path.as_ref().to_path_buf();
        let file = open_lock_file(&path, true)?;
        let (device, inode) = identify(&file)?;
        Ok(Self {
            path,
            device,
            inode,
        })
    }

    /// Take the exclusive lock for this entrant.
    ///
    /// Each entrant opens its own file description: `flock` locks are attached
    /// to the description, so sharing the original descriptor would let two
    /// guards in one process both succeed.
    pub fn try_lock(&self) -> Result<WriterGuard, GateError> {
        let file = open_lock_file(&self.path, false)?;
        let identity = identify(&file)?;
        if identity != (self.device, self.inode) {
            return Err(GateError::Unavailable);
        }

        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result == 0 {
            return Ok(WriterGuard { _file: file });
        }

        match io::Error::last_os_error().raw_os_error() {
            Some(libc::EWOULDBLOCK) => Err(GateError::Busy),
            _ => Err(GateError::Io),
        }
    }
}

fn open_lock_file(path: &Path, create: bool) -> Result<File, GateError> {
    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| GateError::Unavailable)?;
    let mut flags = libc::O_RDWR | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    if create {
        flags |= libc::O_CREAT;
    }

    let descriptor = unsafe { libc::open(path.as_ptr(), flags, 0o600 as libc::mode_t) };
    if descriptor < 0 {
        return Err(open_error(io::Error::last_os_error()));
    }
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

fn identify(file: &File) -> Result<(u64, u64), GateError> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::fstat(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(GateError::Io);
    }
    let stat = unsafe { stat.assume_init() };

    if stat.st_mode & libc::S_IFMT != libc::S_IFREG || stat.st_nlink != 1 {
        return Err(GateError::Unavailable);
    }
    Ok((stat.st_dev as u64, stat.st_ino as u64))
}

fn open_error(error: io::Error) -> GateError {
    match error.raw_os_error() {
        Some(libc::ELOOP) | Some(libc::ENOENT) | Some(libc::ENOTDIR) => GateError::Unavailable,
        _ => GateError::Io,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_is_exclusive_across_entrants() {
        let directory = tempfile::tempdir().expect("scratch directory");
        let path = directory.path().join("library.lock");

        let gate = WriterGate::open(&path).expect("open gate");
        let first = gate.try_lock().expect("first lock");
        assert!(matches!(gate.try_lock(), Err(GateError::Busy)));

        let second_gate = WriterGate::open(&path).expect("reopen gate");
        assert!(matches!(second_gate.try_lock(), Err(GateError::Busy)));

        drop(first);
        assert!(second_gate.try_lock().is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn gate_refuses_symlink() {
        let directory = tempfile::tempdir().expect("scratch directory");
        let target = directory.path().join("real.lock");
        std::fs::write(&target, []).expect("target");
        let link = directory.path().join("link.lock");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        assert!(matches!(
            WriterGate::open(&link),
            Err(GateError::Unavailable)
        ));
    }
}
