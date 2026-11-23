//! Cross-catalog installation lease.
//!
//! One record per physical installation, stored under the user's state
//! directory rather than under any catalog's app-data root, so every catalog
//! (every `XDG_DATA_HOME` or portable configuration) of one user sees it. The
//! writer gate is held only while the record is examined or replaced; the
//! ownership it records outlives every process by design.
//!
//! The gate and the record are separate objects: `<key>.lock` carries no data
//! and is only ever locked, while `<key>.lease` holds the record and is read
//! and written through ordinary file handles. Windows byte-range locks are
//! mandatory, so gating on the record itself would make the second handle fail
//! with `ERROR_LOCK_VIOLATION`; the separate lock object keeps the lock off the
//! data path on every platform. The lock file stays after release, and a
//! crashed claim leaves it behind, but neither is a record.
//!
//! The record names the owning catalog root and the operation or generation it
//! holds. A present-but-unparseable record is a typed refusal: it is never
//! read as absent. Absence of the record is the only vacant state; an empty
//! file is a crash artifact, not an invitation to claim.

use std::fmt;
use std::fs::File;
use std::path::{Path, PathBuf};

use crate::{GateError, WriterGate};
use marduk_core::root_identity::RootError;

const RECORD_PREFIX: &str = "marduk-lease/1";

/// The owning catalog root and the reference it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeaseRecord {
    /// The app-data root of the catalog that owns the installation.
    pub app_data: PathBuf,
    /// `plan:<id>`, `generation:<id>` or another local reference. `None` only
    /// for a record written without a reference.
    pub reference: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseError {
    /// Another examination holds the gate; nothing is known about the record.
    Busy,
    /// The lease object or its record cannot be trusted or parsed.
    Unreadable,
    /// The operating system failed.
    Io,
}

impl fmt::Display for LeaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Busy => "another process is examining the installation lease",
            Self::Unreadable => "the installation lease record is unreadable",
            Self::Io => "the installation lease operation failed",
        })
    }
}

impl std::error::Error for LeaseError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LeaseClaim {
    /// The record is absent or already names this catalog, and now names it
    /// with the supplied reference.
    Acquired,
    /// The record names another catalog root.
    Foreign(PathBuf),
}

/// One installation lease pinned to its verified directory.
///
/// `<key>.lock` is the gate and never carries data; `<key>.lease` is the
/// record. The identities live in one verified directory handle, so both
/// objects share its containment checks without sharing a data path.
pub struct InstallationLease {
    dir: File,
    dir_path: PathBuf,
    lock: String,
    record: String,
}

impl InstallationLease {
    /// Open the lease directory, creating it (and its parents) when absent.
    pub fn open(leases: &Path, key: &str) -> Result<Self, LeaseError> {
        let dir = crate::open_private_directory(leases).map_err(root_error)?;
        Self::pin(dir, leases, key)
    }

    /// Open the lease directory only when it already exists. A missing
    /// directory has no record and creates nothing.
    pub fn open_existing(leases: &Path, key: &str) -> Result<Option<Self>, LeaseError> {
        match crate::open_existing_private_directory(leases) {
            Ok(dir) => Self::pin(dir, leases, key).map(Some),
            Err(RootError::TargetUnavailable) => Ok(None),
            Err(error) => Err(root_error(error)),
        }
    }

    fn pin(dir: File, dir_path: &Path, key: &str) -> Result<Self, LeaseError> {
        if key.len() != 64
            || !key
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err(LeaseError::Unreadable);
        }
        #[cfg(target_os = "linux")]
        if !crate::private_directory_is_owner_only(&dir).map_err(root_error)? {
            return Err(LeaseError::Unreadable);
        }
        Ok(Self {
            dir,
            dir_path: dir_path.to_path_buf(),
            lock: format!("{key}.lock"),
            record: format!("{key}.lease"),
        })
    }

    /// Read the record under the gate. `Ok(None)` means the record is vacant.
    pub fn read(&self) -> Result<Option<LeaseRecord>, LeaseError> {
        let gate = self.gate()?;
        let _guard = gate.try_lock().map_err(gate_error)?;
        read_locked(&self.dir, &self.record)
    }

    /// Claim the installation for `record`, or report the foreign owner.
    ///
    /// Examination and replacement happen under one held gate, so two catalogs
    /// cannot both observe a vacant record and then both write. The new bytes
    /// reach a fresh sibling first and one atomic replace publishes them, so the
    /// live record is never truncated: a cut leaves the previous record or the
    /// new one.
    pub fn claim(&self, record: &LeaseRecord) -> Result<LeaseClaim, LeaseError> {
        let gate = self.gate()?;
        let _guard = gate.try_lock().map_err(gate_error)?;
        if let Some(current) = read_locked(&self.dir, &self.record)? {
            if current.app_data != record.app_data {
                return Ok(LeaseClaim::Foreign(current.app_data));
            }
        }
        self.publish(&encode(record))?;
        Ok(LeaseClaim::Acquired)
    }

    /// Clear the record only when it names `app_data`, under one held gate.
    /// Returns whether this catalog owned it. Removal is an unlink, never an
    /// empty write: absence is the one vacant state.
    pub fn release(&self, app_data: &Path) -> Result<bool, LeaseError> {
        self.clear_matching(|current| current.app_data == app_data)
    }

    /// Clear the record only when it names both `app_data` and `reference`,
    /// under one held gate. `None` matches only a record without a reference, so
    /// an older call cannot clear a newer claim. Returns whether it was removed.
    pub fn clear_if(&self, app_data: &Path, reference: Option<&str>) -> Result<bool, LeaseError> {
        self.clear_matching(|current| {
            current.app_data == app_data && current.reference.as_deref() == reference
        })
    }

    /// Write `bytes` to a fresh sibling, then replace the record with it in one
    /// namespace operation and flush the directory. A leftover sibling is not a
    /// record; a later claim removes it before creating its own.
    fn publish(&self, bytes: &[u8]) -> Result<(), LeaseError> {
        let temporary = format!("{}.tmp", self.record);
        crate::native::remove_private_file(&self.dir, &temporary).map_err(root_error)?;
        crate::native::create_private_file(&self.dir, &temporary, bytes).map_err(root_error)?;
        crate::native::replace_private_file(&self.dir, &temporary, &self.record)
            .map_err(root_error)?;
        crate::effects::sync_bound_directory(&self.dir, &self.dir_path).map_err(root_error)
    }

    fn clear_matching(
        &self,
        matches: impl FnOnce(&LeaseRecord) -> bool,
    ) -> Result<bool, LeaseError> {
        let gate = self.gate()?;
        let _guard = gate.try_lock().map_err(gate_error)?;
        match read_locked(&self.dir, &self.record)? {
            Some(current) if matches(&current) => {
                crate::native::remove_private_file(&self.dir, &self.record).map_err(root_error)?;
                crate::effects::sync_bound_directory(&self.dir, &self.dir_path)
                    .map_err(root_error)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// Open the data-free lock object for this installation.
    fn gate(&self) -> Result<WriterGate, LeaseError> {
        WriterGate::open_at(&self.dir, &self.lock).map_err(gate_error)
    }
}

fn read_locked(dir: &File, name: &str) -> Result<Option<LeaseRecord>, LeaseError> {
    let bytes = crate::read_private_file(dir, name).map_err(root_error)?;
    decode(bytes.as_deref())
}

fn gate_error(error: GateError) -> LeaseError {
    match error {
        GateError::Busy => LeaseError::Busy,
        GateError::Unavailable => LeaseError::Unreadable,
        GateError::Io => LeaseError::Io,
    }
}

fn root_error(error: RootError) -> LeaseError {
    match error {
        RootError::UnsupportedTarget(_) => LeaseError::Unreadable,
        _ => LeaseError::Io,
    }
}

fn encode(record: &LeaseRecord) -> Vec<u8> {
    let mut text = format!(
        "{RECORD_PREFIX}\n{}\n{}\n",
        hex(&path_bytes(&record.app_data)),
        record
            .reference
            .as_deref()
            .map(|reference| hex(reference.as_bytes()))
            .unwrap_or_else(|| "-".to_owned()),
    )
    .into_bytes();
    text.shrink_to_fit();
    text
}

fn decode(bytes: Option<&[u8]>) -> Result<Option<LeaseRecord>, LeaseError> {
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    // A present file with no bytes is the exact shape a truncate-then-write cut
    // leaves; treating it as vacant would let a second catalog claim a
    // deployment that still exists.
    if bytes.is_empty() {
        return Err(LeaseError::Unreadable);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| LeaseError::Unreadable)?;
    let mut lines = text.split('\n');
    if lines.next() != Some(RECORD_PREFIX) {
        return Err(LeaseError::Unreadable);
    }
    let path = lines.next().ok_or(LeaseError::Unreadable)?;
    let reference = lines.next().ok_or(LeaseError::Unreadable)?;
    if lines.next() != Some("") || lines.next().is_some() {
        return Err(LeaseError::Unreadable);
    }
    let reference = if reference == "-" {
        None
    } else {
        Some(String::from_utf8(unhex(reference)?).map_err(|_| LeaseError::Unreadable)?)
    };
    Ok(Some(LeaseRecord {
        app_data: path_from_bytes(unhex(path)?)?,
        reference,
    }))
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

fn unhex(text: &str) -> Result<Vec<u8>, LeaseError> {
    if !text.len().is_multiple_of(2) {
        return Err(LeaseError::Unreadable);
    }
    let bytes = text.as_bytes();
    let mut output = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        let high = (pair[0] as char)
            .to_digit(16)
            .ok_or(LeaseError::Unreadable)?;
        let low = (pair[1] as char)
            .to_digit(16)
            .ok_or(LeaseError::Unreadable)?;
        output.push((high * 16 + low) as u8);
    }
    Ok(output)
}

#[cfg(unix)]
fn path_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(unix)]
fn path_from_bytes(bytes: Vec<u8>) -> Result<PathBuf, LeaseError> {
    use std::os::unix::ffi::OsStringExt;
    Ok(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

#[cfg(windows)]
fn path_bytes(path: &Path) -> Vec<u8> {
    path.to_string_lossy().as_bytes().to_vec()
}

#[cfg(windows)]
fn path_from_bytes(bytes: Vec<u8>) -> Result<PathBuf, LeaseError> {
    String::from_utf8(bytes)
        .map(PathBuf::from)
        .map_err(|_| LeaseError::Unreadable)
}

/// The one lease directory every catalog of this user shares.
///
/// Windows: `%LOCALAPPDATA%\marduk\leases`. Linux:
/// `$XDG_STATE_HOME/marduk/leases`, else
/// `$HOME/.local/state/marduk/leases`. No app-data root participates, so a
/// different catalog cannot hide from the record.
#[cfg(windows)]
pub fn installation_lease_dir() -> Result<PathBuf, RootError> {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .map(|path| path.join("marduk").join("leases"))
        .ok_or(RootError::TargetUnavailable)
}

#[cfg(not(windows))]
pub fn installation_lease_dir() -> Result<PathBuf, RootError> {
    #[cfg(target_os = "linux")]
    {
        if let Some(state) = std::env::var_os("XDG_STATE_HOME") {
            let state = PathBuf::from(state);
            if state.is_absolute() {
                return Ok(state.join("marduk").join("leases"));
            }
        }
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|home| home.is_absolute())
            .map(|home| {
                home.join(".local")
                    .join("state")
                    .join("marduk")
                    .join("leases")
            })
            .ok_or(RootError::TargetUnavailable);
    }
    #[cfg(not(target_os = "linux"))]
    Err(RootError::TargetUnavailable)
}
