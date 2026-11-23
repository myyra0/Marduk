use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use marduk_core::deployment::DeployError;

use crate::{InstallationBinding, effects};

pub const DEPLOYMENT_MARKER: &str = ".marduk-deployment";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeploymentOwner {
    Vacant,
    Owned(PathBuf),
    Foreign(PathBuf),
}

impl InstallationBinding {
    pub fn deployment_owner(&self, catalog: &Path) -> Result<DeploymentOwner, DeployError> {
        self.verify().map_err(marker_root_error)?;
        let (_, root) = self.inner.installation_directory();
        read_owner(&root, catalog)
    }

    /// Publish a complete claim or report the owner that already holds it.
    pub fn claim_deployment(&self, catalog: &Path) -> Result<DeploymentOwner, DeployError> {
        self.verify().map_err(marker_root_error)?;
        let (directory, root) = self.inner.installation_directory();
        match read_owner(&root, catalog)? {
            DeploymentOwner::Vacant => {}
            owner @ DeploymentOwner::Owned(_) => {
                effects::sync_bound_directory(directory, &root).map_err(|_| DeployError::Io)?;
                return Ok(owner);
            }
            owner => return Ok(owner),
        }

        let record = encode_owner(catalog)?;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let (name, mut file) = loop {
            let name = format!(
                "{DEPLOYMENT_MARKER}.tmp-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            );
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(root.join(&name))
            {
                Ok(file) => break (name, file),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(DeployError::Io),
            }
        };

        let written = file.write_all(&record).and_then(|()| file.sync_all());
        drop(file);
        let publish = written
            .and_then(|()| effects::move_noreplace_in(directory, &root, &name, DEPLOYMENT_MARKER));
        match publish {
            Ok(()) => {
                effects::sync_bound_directory(directory, &root).map_err(|_| DeployError::Io)?;
                Ok(DeploymentOwner::Owned(catalog.to_path_buf()))
            }
            Err(error) => {
                // This process exclusively created the temporary name.
                fs::remove_file(root.join(name)).map_err(|_| DeployError::Io)?;
                if error.kind() == io::ErrorKind::AlreadyExists {
                    return match read_owner(&root, catalog)? {
                        DeploymentOwner::Vacant => Err(DeployError::ActiveDeployment),
                        owner => Ok(owner),
                    };
                }
                Err(DeployError::Io)
            }
        }
    }

    /// Clear this catalog's marker. A foreign or unreadable marker is left intact.
    pub fn clear_deployment(&self, catalog: &Path) -> Result<(), DeployError> {
        self.verify().map_err(marker_root_error)?;
        let (directory, root) = self.inner.installation_directory();
        match read_owner(&root, catalog)? {
            DeploymentOwner::Vacant => Ok(()),
            DeploymentOwner::Foreign(_) => Err(DeployError::ActiveDeployment),
            DeploymentOwner::Owned(_) => {
                fs::remove_file(root.join(DEPLOYMENT_MARKER)).map_err(|_| DeployError::Io)?;
                effects::sync_bound_directory(directory, &root).map_err(|_| DeployError::Io)
            }
        }
    }
}

fn marker_root_error(error: marduk_core::root_identity::RootError) -> DeployError {
    use marduk_core::root_identity::RootError;
    match error {
        RootError::Io => DeployError::Io,
        RootError::UnsupportedTarget(_) => DeployError::UnsupportedTarget,
        _ => DeployError::TargetUnavailable,
    }
}

fn read_owner(root: &Path, catalog: &Path) -> Result<DeploymentOwner, DeployError> {
    let path = root.join(DEPLOYMENT_MARKER);
    let metadata = match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(DeploymentOwner::Vacant);
        }
        Ok(metadata) if metadata.is_file() => metadata,
        _ => return Err(DeployError::RecoveryConflict),
    };
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(DeployError::RecoveryConflict);
        }
    }
    const MAX_RECORD: usize = 128 * 1024;
    if metadata.len() > MAX_RECORD as u64 {
        return Err(DeployError::RecoveryConflict);
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .and_then(|file| file.take(MAX_RECORD as u64 + 1).read_to_end(&mut bytes))
        .map_err(|_| DeployError::RecoveryConflict)?;
    let recorded = decode_owner(&bytes)
        .filter(|path| bytes.len() <= MAX_RECORD && path.is_absolute())
        .ok_or(DeployError::RecoveryConflict)?;
    Ok(if recorded == catalog {
        DeploymentOwner::Owned(recorded)
    } else {
        DeploymentOwner::Foreign(recorded)
    })
}

#[cfg(unix)]
fn encode_owner(path: &Path) -> Result<Vec<u8>, DeployError> {
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes();
    if !path.is_absolute() || bytes.contains(&0) {
        return Err(DeployError::Io);
    }
    let mut record = bytes.to_vec();
    record.push(0);
    Ok(record)
}

#[cfg(unix)]
fn decode_owner(record: &[u8]) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let path = record.strip_suffix(&[0])?;
    (!path.contains(&0)).then(|| PathBuf::from(std::ffi::OsStr::from_bytes(path)))
}

#[cfg(windows)]
fn encode_owner(path: &Path) -> Result<Vec<u8>, DeployError> {
    use std::os::windows::ffi::OsStrExt;
    if !path.is_absolute() {
        return Err(DeployError::Io);
    }
    let mut record = Vec::new();
    for unit in path.as_os_str().encode_wide() {
        if unit == 0 {
            return Err(DeployError::Io);
        }
        record.extend(unit.to_le_bytes());
    }
    record.extend([0, 0]);
    Ok(record)
}

#[cfg(windows)]
fn decode_owner(record: &[u8]) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    let (units, tail) = record.strip_suffix(&[0, 0])?.as_chunks::<2>();
    let units: Vec<_> = units.iter().copied().map(u16::from_le_bytes).collect();
    (tail.is_empty() && !units.contains(&0))
        .then(|| PathBuf::from(std::ffi::OsString::from_wide(&units)))
}
