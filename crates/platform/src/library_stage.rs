//! Verified private-library staging.
//!
//! One concrete adapter issues opaque [`NativeReceipt`]s. Storage consumes the
//! receipt; no caller can construct evidence, and an unqualified volume yields
//! a typed refusal instead of a fake success.
//!
//! The archive snapshot, admitted member set and staged payload are all inside
//! one owned revision directory. Publication never reopens a caller-selected
//! ZIP path.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use marduk_archive::{M1_ZIP_LIMITS, admit_zip};
use marduk_core::import::{
    ArchiveError, ImportAllocation, ImportError, LimitKind, PayloadFile, StageEvidence,
};
use marduk_core::root_identity::{RootError, RootIdentity};
use marduk_core::targets::{RootRole, TargetFile};
use sha2::{Digest, Sha256};

use crate::WriterGuard;

const REVISIONS_DIR: &str = "revisions";
const ARCHIVE_FILE: &str = "archive.zip";
const PAYLOAD_DIR: &str = "payload";

/// Opaque qualified issuance. Only [`NativeStage`] can build one.
pub struct NativeReceipt {
    evidence: StageEvidence,
}

impl NativeReceipt {
    pub fn evidence(&self) -> &StageEvidence {
        &self.evidence
    }
}

/// The one concrete native library adapter.
pub struct NativeStage {
    root: PathBuf,
}

impl NativeStage {
    /// Connect only the volume classes this packet qualifies. Everything else
    /// stays disconnected with `StageUnavailable`.
    pub fn open(root: &Path) -> Result<Self, ImportError> {
        if !fsops::qualified(root) {
            return Err(ImportError::StageUnavailable);
        }
        fsops::directory_identity(root).map_err(|_| ImportError::StageUnavailable)?;
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    /// Take a bounded private snapshot, admit it, stream its members and record
    /// durable evidence. The live writer guard is the caller's library lease.
    pub fn stage(
        &mut self,
        _guard: &WriterGuard,
        allocation: ImportAllocation,
        input: &mut dyn Read,
        record_owner: &mut dyn FnMut(&RootIdentity) -> Result<(), ImportError>,
    ) -> Result<NativeReceipt, ImportError> {
        let directory = self
            .root
            .join(REVISIONS_DIR)
            .join(allocation.id.get().to_string());
        fs::create_dir(&directory).map_err(|_| ImportError::StageUnavailable)?;
        let owner =
            fsops::directory_identity(&directory).map_err(|_| ImportError::StageUnavailable)?;
        record_owner(&owner)?;

        let archive = directory.join(ARCHIVE_FILE);
        let archive_sha256 = copy_archive(input, &archive)?;
        let files = extract(&directory, &archive)?;
        let evidence = StageEvidence {
            allocation,
            archive_sha256,
            files,
            owner,
        };
        verify_tree(&directory, &evidence)?;
        flush(&self.root, &directory, &evidence)?;
        Ok(NativeReceipt { evidence })
    }

    /// Re-open the exact owned revision directory and re-verify the archive and
    /// payload against persisted evidence. SQL is not proof of a payload entry.
    pub fn reobserve(
        &mut self,
        _guard: &WriterGuard,
        evidence: &StageEvidence,
    ) -> Result<(), ImportError> {
        let directory = self.revision_dir(evidence);
        verify_tree(&directory, evidence)
    }

    /// Reclaim only a fully re-verified owned stage. Changed or unavailable data
    /// is preserved.
    pub fn reclaim(
        &mut self,
        _guard: &WriterGuard,
        evidence: &StageEvidence,
    ) -> Result<(), ImportError> {
        let directory = self.revision_dir(evidence);
        match fs::symlink_metadata(&directory) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(ImportError::StageUnavailable),
            Ok(_) => {}
        }
        verify_tree(&directory, evidence)?;

        remove_file_if_present(&directory.join(ARCHIVE_FILE))?;
        for file in &evidence.files {
            remove_file_if_present(&payload_path(&directory, file.target.relative.as_str()))?;
        }
        prune(&directory, evidence)?;
        fsops::sync_dir(&self.root).map_err(|_| ImportError::Storage)
    }

    fn revision_dir(&self, evidence: &StageEvidence) -> PathBuf {
        self.root
            .join(REVISIONS_DIR)
            .join(evidence.allocation.id.get().to_string())
    }
}

fn copy_archive(input: &mut dyn Read, path: &Path) -> Result<[u8; 32], ImportError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| ImportError::StageUnavailable)?;
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = input
            .read(&mut buffer)
            .map_err(|_| ImportError::Archive(ArchiveError::Io))?;
        if read == 0 {
            break;
        }
        total = total.checked_add(read as u64).ok_or(ImportError::Archive(
            ArchiveError::LimitExceeded {
                kind: LimitKind::ArchiveBytes,
            },
        ))?;
        if total > M1_ZIP_LIMITS.max_archive_bytes {
            return Err(ImportError::Archive(ArchiveError::LimitExceeded {
                kind: LimitKind::ArchiveBytes,
            }));
        }
        hasher.update(&buffer[..read]);
        file.write_all(&buffer[..read])
            .map_err(|_| ImportError::Storage)?;
    }
    file.sync_all().map_err(|_| ImportError::Storage)?;
    Ok(hasher.finalize().into())
}

fn extract(directory: &Path, archive: &Path) -> Result<Vec<PayloadFile>, ImportError> {
    let file = fsops::open_regular(archive).map_err(|_| ImportError::StageUnavailable)?;
    let mut admitted = admit_zip(file, M1_ZIP_LIMITS).map_err(ImportError::Archive)?;

    let mut files = Vec::with_capacity(admitted.entries().len());
    for index in 0..admitted.entries().len() {
        let (relative, declared_len) = {
            let entry = &admitted.entries()[index];
            (entry.path.clone(), entry.uncompressed_len)
        };
        let path = payload_path(directory, relative.as_str());
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|_| ImportError::Storage)?;
        }
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|_| ImportError::StageChanged)?;
        let mut writer = HashingFile::new(file);
        let written = admitted
            .copy_entry(index, &mut writer)
            .map_err(ImportError::Archive)?;
        if written != declared_len {
            return Err(ImportError::StageChanged);
        }
        let (sha256, file) = writer.finish();
        file.sync_all().map_err(|_| ImportError::Storage)?;
        files.push(PayloadFile {
            target: TargetFile {
                root: RootRole::Data,
                relative,
            },
            len: written,
            sha256,
        });
    }
    Ok(files)
}

fn flush(root: &Path, directory: &Path, evidence: &StageEvidence) -> Result<(), ImportError> {
    fsops::sync_file(&directory.join(ARCHIVE_FILE))?;
    for file in &evidence.files {
        fsops::sync_file(&payload_path(directory, file.target.relative.as_str()))?;
    }
    sync_ancestors(&directory.join(ARCHIVE_FILE), root)?;
    for file in &evidence.files {
        sync_ancestors(
            &payload_path(directory, file.target.relative.as_str()),
            root,
        )?;
    }
    fsops::sync_dir(directory).map_err(|_| ImportError::Storage)
}

fn sync_ancestors(path: &Path, root: &Path) -> Result<(), ImportError> {
    let mut current = path.parent();
    while let Some(directory) = current {
        fsops::sync_dir(directory).map_err(|_| ImportError::Storage)?;
        if directory == root {
            break;
        }
        current = directory.parent();
    }
    Ok(())
}

fn verify_tree(directory: &Path, evidence: &StageEvidence) -> Result<(), ImportError> {
    match fsops::directory_identity(directory) {
        Ok(actual) if actual == evidence.owner => {}
        Ok(_) => return Err(ImportError::StageChanged),
        Err(RootError::TargetUnavailable) => return Err(ImportError::StageUnavailable),
        Err(_) => return Err(ImportError::StageChanged),
    }

    let archive = directory.join(ARCHIVE_FILE);
    let (archive_sha256, _) = hash_regular(&archive)?;
    if archive_sha256 != evidence.archive_sha256 {
        return Err(ImportError::StageChanged);
    }
    let archive_file = fsops::open_regular(&archive).map_err(|_| ImportError::StageChanged)?;
    admit_zip(archive_file, M1_ZIP_LIMITS).map_err(|_| ImportError::StageChanged)?;

    let payload = directory.join(PAYLOAD_DIR);
    let mut top_level = BTreeSet::new();
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(ImportError::StageUnavailable);
        }
        Err(_) => return Err(ImportError::StageChanged),
    };
    for entry in entries {
        let entry = entry.map_err(|_| ImportError::StageChanged)?;
        let name = entry
            .file_name()
            .to_str()
            .map(str::to_owned)
            .ok_or(ImportError::StageChanged)?;
        let kind = entry.file_type().map_err(|_| ImportError::StageChanged)?;
        match (name.as_str(), kind.is_file(), kind.is_dir()) {
            (ARCHIVE_FILE, true, false) => {}
            (PAYLOAD_DIR, false, true) => {}
            _ => return Err(ImportError::StageChanged),
        }
        top_level.insert(name);
    }
    if !top_level.contains(ARCHIVE_FILE) {
        return Err(ImportError::StageChanged);
    }

    let mut found = BTreeSet::new();
    if payload.is_dir() {
        collect_files(&payload, "", &mut found)?;
    } else if !evidence.files.is_empty() {
        return Err(ImportError::StageChanged);
    }

    let mut expected = BTreeSet::new();
    for file in &evidence.files {
        if !expected.insert(file.target.relative.as_str().to_owned()) {
            return Err(ImportError::StageChanged);
        }
        let path = payload_path(directory, file.target.relative.as_str());
        let (sha256, len) = hash_regular(&path)?;
        if sha256 != file.sha256 || len != file.len {
            return Err(ImportError::StageChanged);
        }
    }
    if found != expected {
        return Err(ImportError::StageChanged);
    }

    match fsops::directory_identity(directory) {
        Ok(actual) if actual == evidence.owner => Ok(()),
        Err(RootError::TargetUnavailable) => Err(ImportError::StageUnavailable),
        _ => Err(ImportError::StageChanged),
    }
}

fn collect_files(
    root: &Path,
    prefix: &str,
    found: &mut BTreeSet<String>,
) -> Result<(), ImportError> {
    let entries = fs::read_dir(root).map_err(|_| ImportError::StageChanged)?;
    for entry in entries {
        let entry = entry.map_err(|_| ImportError::StageChanged)?;
        let name = entry
            .file_name()
            .to_str()
            .map(str::to_owned)
            .ok_or(ImportError::StageChanged)?;
        let relative = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let kind = entry.file_type().map_err(|_| ImportError::StageChanged)?;
        if kind.is_dir() {
            collect_files(&entry.path(), &relative, found)?;
        } else if kind.is_file() {
            found.insert(relative);
        } else {
            return Err(ImportError::StageChanged);
        }
    }
    Ok(())
}

fn hash_regular(path: &Path) -> Result<([u8; 32], u64), ImportError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => return Err(ImportError::StageChanged),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(ImportError::StageUnavailable);
        }
        Err(_) => return Err(ImportError::StageChanged),
    }
    let mut file = fsops::open_regular(path).map_err(|_| ImportError::StageChanged)?;
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| ImportError::StageChanged)?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(read as u64)
            .ok_or(ImportError::StageChanged)?;
        hasher.update(&buffer[..read]);
    }
    Ok((hasher.finalize().into(), total))
}

fn prune(directory: &Path, evidence: &StageEvidence) -> Result<(), ImportError> {
    let mut deep = vec![directory.join(PAYLOAD_DIR)];
    for file in &evidence.files {
        let mut path = directory.join(PAYLOAD_DIR);
        let components = file.target.relative.as_str().split('/').collect::<Vec<_>>();
        for component in &components[..components.len().saturating_sub(1)] {
            path.push(component);
            deep.push(path.clone());
        }
    }
    deep.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    deep.dedup();
    for path in deep {
        if path == directory {
            continue;
        }
        match fs::remove_dir(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) if error.kind() == io::ErrorKind::DirectoryNotEmpty => {
                return Err(ImportError::StageChanged);
            }
            Err(_) => return Err(ImportError::Storage),
        }
    }
    match fs::remove_dir(directory) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::DirectoryNotEmpty => {
            Err(ImportError::StageChanged)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(ImportError::Storage),
    }
}

fn remove_file_if_present(path: &Path) -> Result<(), ImportError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(ImportError::Storage),
    }
}

fn payload_path(directory: &Path, relative: &str) -> PathBuf {
    let mut path = directory.join(PAYLOAD_DIR);
    for component in relative.split('/') {
        path.push(component);
    }
    path
}

struct HashingFile {
    file: File,
    hasher: Sha256,
}

impl HashingFile {
    fn new(file: File) -> Self {
        Self {
            file,
            hasher: Sha256::new(),
        }
    }

    fn finish(self) -> ([u8; 32], File) {
        (self.hasher.finalize().into(), self.file)
    }
}

impl Write for HashingFile {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let written = self.file.write(buffer)?;
        self.hasher.update(&buffer[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[cfg(target_os = "linux")]
mod fsops {
    use super::*;

    pub fn qualified(root: &Path) -> bool {
        crate::native::library_volume_qualified(root)
    }

    pub fn directory_identity(path: &Path) -> Result<RootIdentity, RootError> {
        crate::native::directory_identity(path)
    }

    pub fn open_regular(path: &Path) -> Result<File, RootError> {
        crate::native::open_file(path)
    }

    pub fn sync_file(path: &Path) -> Result<(), ImportError> {
        crate::native::open_file(path)
            .and_then(|file| file.sync_all().map_err(|_| RootError::Io))
            .map_err(|_| ImportError::Storage)
    }

    pub fn sync_dir(path: &Path) -> Result<(), RootError> {
        crate::native::sync_directory(path)
    }
}

#[cfg(windows)]
mod fsops {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

    pub fn qualified(_root: &Path) -> bool {
        false
    }

    pub fn directory_identity(path: &Path) -> Result<RootIdentity, RootError> {
        crate::native::directory_identity(path)
    }

    pub fn open_regular(path: &Path) -> Result<File, RootError> {
        OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .map_err(|_| RootError::TargetUnavailable)
    }

    pub fn sync_file(path: &Path) -> Result<(), ImportError> {
        open_regular(path)
            .and_then(|file| file.sync_all().map_err(|_| RootError::Io))
            .map_err(|_| ImportError::Storage)
    }

    pub fn sync_dir(_path: &Path) -> Result<(), RootError> {
        // No ordinary-token directory barrier is qualified on Windows.
        Ok(())
    }
}
