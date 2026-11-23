//! One-client instance gate.
//!
//! The gate is a fixed per-user lock file, never derived from a catalog root,
//! the game folder or the current directory, so every Marduk client of one OS
//! user contends on the same object. It is held for the client's lifetime; a
//! second acquisition is refused with [`GateError::Busy`] and never blocks.

use std::path::PathBuf;

use crate::{GateError, WriterGate, WriterGuard};

/// Held instance gate. Dropping it (or exiting the process) releases the lock.
pub struct InstanceGuard {
    _guard: WriterGuard,
}

/// Take the per-user instance gate without waiting.
pub fn acquire_instance_guard() -> Result<InstanceGuard, GateError> {
    let path = instance_lock_path()?;
    let gate = WriterGate::open(path)?;
    gate.try_lock().map(|guard| InstanceGuard { _guard: guard })
}

/// `%LOCALAPPDATA%\marduk\instance.lock`, creating the parent first.
#[cfg(windows)]
fn instance_lock_path() -> Result<PathBuf, GateError> {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or(GateError::Unavailable)?;
    let directory = base.join("marduk");
    crate::open_private_directory(&directory).map_err(|_| GateError::Unavailable)?;
    Ok(directory.join("instance.lock"))
}

/// `$XDG_RUNTIME_DIR/marduk/instance.lock`, else
/// `/tmp/marduk-instance-<uid>.lock`. The runtime parent is created `0700`.
#[cfg(target_os = "linux")]
fn instance_lock_path() -> Result<PathBuf, GateError> {
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        let directory = runtime.join("marduk");
        crate::open_private_directory(&directory).map_err(|_| GateError::Unavailable)?;
        return Ok(directory.join("instance.lock"));
    }
    let uid = unsafe { libc::getuid() };
    Ok(PathBuf::from("/tmp").join(format!("marduk-instance-{uid}.lock")))
}
