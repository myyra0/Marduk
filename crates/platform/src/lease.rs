//! Cross-catalog installation lease.
//!
//! One record per physical installation, stored under the user's state
//! directory rather than under any catalog's app-data root, so every catalog
//! (every `XDG_DATA_HOME` or portable configuration) of one user sees it. The
//! writer gate is held only while the record is examined or replaced; the
//! ownership it records outlives every process by design.
//!
//! The record names the owning catalog root and the operation or generation it
//! holds. A present-but-unparseable record is a typed refusal: it is never
//! read as absent.

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

/// One installation lease file pinned to its verified directory.
pub struct InstallationLease {
    dir: File,
    name: String,
}

impl InstallationLease {
    /// Open the lease directory, creating it (and its parents) when absent.
    pub fn open(leases: &Path, key: &str) -> Result<Self, LeaseError> {
        let dir = crate::open_private_directory(leases).map_err(root_error)?;
        Self::pin(dir, key)
    }

    /// Open the lease directory only when it already exists. A missing
    /// directory has no record and creates nothing.
    pub fn open_existing(leases: &Path, key: &str) -> Result<Option<Self>, LeaseError> {
        match crate::open_existing_private_directory(leases) {
            Ok(dir) => Self::pin(dir, key).map(Some),
            Err(RootError::TargetUnavailable) => Ok(None),
            Err(error) => Err(root_error(error)),
        }
    }

    fn pin(dir: File, key: &str) -> Result<Self, LeaseError> {
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
            name: format!("{key}.lease"),
        })
    }

    /// Read the record under the gate. `Ok(None)` means the record is vacant.
    pub fn read(&self) -> Result<Option<LeaseRecord>, LeaseError> {
        let gate = self.gate()?;
        let _guard = gate.try_lock().map_err(gate_error)?;
        read_locked(&self.dir, &self.name)
    }

    /// Claim the installation for `record`, or report the foreign owner.
    ///
    /// Examination and replacement happen under one held gate, so two catalogs
    /// cannot both observe a vacant record and then both write.
    pub fn claim(&self, record: &LeaseRecord) -> Result<LeaseClaim, LeaseError> {
        let gate = self.gate()?;
        let _guard = gate.try_lock().map_err(gate_error)?;
        if let Some(current) = read_locked(&self.dir, &self.name)? {
            if current.app_data != record.app_data {
                return Ok(LeaseClaim::Foreign(current.app_data));
            }
        }
        crate::write_private_file(&self.dir, &self.name, &encode(record)).map_err(root_error)?;
        Ok(LeaseClaim::Acquired)
    }

    /// Clear the record only when it names `app_data`, under one held gate.
    /// Returns whether this catalog owned it. The lease file itself stays, so
    /// a gate opened before the clear keeps a stable identity.
    pub fn release(&self, app_data: &Path) -> Result<bool, LeaseError> {
        let gate = self.gate()?;
        let _guard = gate.try_lock().map_err(gate_error)?;
        match read_locked(&self.dir, &self.name)? {
            Some(current) if current.app_data == app_data => {
                crate::write_private_file(&self.dir, &self.name, b"").map_err(root_error)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn gate(&self) -> Result<WriterGate, LeaseError> {
        WriterGate::open_at(&self.dir, &self.name).map_err(gate_error)
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
    if bytes.is_empty() {
        return Ok(None);
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
