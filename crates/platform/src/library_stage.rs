//! Verified private-library staging.
//!
//! One concrete adapter issues opaque [`NativeReceipt`]s. Storage consumes the
//! receipt; no caller can construct a receipt, and an unqualified volume yields
//! a typed refusal instead of a fake success. Availability is recorded in
//! `docs/research/m1-native-results.md`; the refusal rule is owned by
//! `docs/contracts/m1-recovery.md`.
//!
//! The archive snapshot, admitted member set and staged payload are all inside
//! one owned revision directory. Publication never reopens a caller-selected
//! ZIP path.
//!
//! On Windows the library root, `revisions/` and every bound revision are opened
//! as no-follow directory handles that deny delete sharing, and every child
//! operation is a relative `NtCreateFile` (or handle disposition) beneath one of
//! those handles. The verified directory names therefore cannot be renamed or
//! replaced while the stage is bound. Payload and archive-entry operations are
//! handle-relative; bookkeeping re-opens (`reverify_revisions`,
//! `verify_revision_path`, `sync_dir`) are pathname-based and re-proved
//! immediately. The remaining residual is inside a revision: a same-privilege
//! process can still add, delete or rewrite payload files between the last
//! re-proof and receipt issuance, which the next re-observe detects. See
//! `docs/contracts/m1-recovery.md`.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;

use marduk_archive::{M1_ZIP_LIMITS, admit_zip};
use marduk_core::import::{
    ArchiveError, ImportAllocation, ImportError, LimitKind, PayloadFile, StageEvidence,
};
use marduk_core::root_identity::{RootError, RootIdentity};
use marduk_core::targets::{RootRole, TargetFile};
use sha2::{Digest, Sha256};

use crate::WriterGuard;

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
    tree: fsops::StagingTree,
}

impl NativeStage {
    /// Connect only volumes with recorded runtime evidence; everything else is
    /// `StageUnavailable` (`docs/research/m1-native-results.md`). The catalog
    /// root alone is not the staging boundary: `revisions/` is bound by a live
    /// no-follow handle.
    pub fn open(root: &Path) -> Result<Self, ImportError> {
        if !fsops::qualified(root) {
            return Err(ImportError::StageUnavailable);
        }
        let tree = fsops::bind(root).map_err(|_| ImportError::StageUnavailable)?;
        Ok(Self { tree })
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
        let name = allocation.id.get().to_string();
        let revision = self
            .tree
            .create_revision(&name)
            .map_err(fsops::stage_error)?;
        let owner = revision.identity().clone();
        record_owner(&owner)?;
        // The caller's ownership write can take time; re-prove the bound tree
        // before the first archive byte is written.
        self.tree
            .reverify_revision(&name, &revision)
            .map_err(fsops::stage_error)?;

        // Every byte below is written relative to the revision handle opened
        // above; a racing path swap cannot redirect the archive or payload.
        let archive_sha256 = copy_archive(&revision, input)?;
        let files = extract(&revision)?;
        let evidence = StageEvidence {
            allocation,
            archive_sha256,
            files,
            owner,
        };
        self.tree
            .reverify_revision(&name, &revision)
            .map_err(fsops::stage_error)?;
        verify_tree(&revision, &evidence)?;
        flush(&revision, &evidence)?;
        // The revision entry itself is new, so persist the `revisions/` directory
        // before the receipt can claim a durable stage.
        self.tree.sync_revisions().map_err(fsops::stage_error)?;
        // The receipt is issued only after the bound handles and their paths are
        // re-observed unchanged; content checks alone are not boundary evidence.
        self.tree
            .reverify_revision(&name, &revision)
            .map_err(fsops::stage_error)?;
        Ok(NativeReceipt { evidence })
    }

    /// Re-open the exact owned revision directory and re-verify the archive and
    /// payload against persisted evidence. SQL is not proof of a payload entry.
    pub fn reobserve(
        &mut self,
        _guard: &WriterGuard,
        evidence: &StageEvidence,
    ) -> Result<(), ImportError> {
        let name = evidence.allocation.id.get().to_string();
        let revision = self.tree.reobserve(&name).map_err(fsops::stage_error)?;
        if revision.identity() != &evidence.owner {
            return Err(ImportError::StageChanged);
        }
        verify_tree(&revision, evidence)?;
        self.tree
            .reverify_revision(&name, &revision)
            .map_err(fsops::stage_error)
    }

    /// Reclaim only a fully re-verified owned stage. Changed or unavailable data
    /// is preserved.
    pub fn reclaim(
        &mut self,
        _guard: &WriterGuard,
        evidence: &StageEvidence,
    ) -> Result<(), ImportError> {
        let name = evidence.allocation.id.get().to_string();
        let revision = match self.tree.open_revision(&name) {
            Ok(Some(revision)) => revision,
            Ok(None) => return Ok(()),
            Err(error) => return Err(fsops::stage_error(error)),
        };
        if revision.identity() != &evidence.owner {
            return Err(ImportError::StageChanged);
        }
        self.tree
            .reverify_revision(&name, &revision)
            .map_err(fsops::stage_error)?;

        verify_tree(&revision, evidence)?;

        revision
            .remove_relative_file(&[ARCHIVE_FILE])
            .map_err(|_| ImportError::Storage)?;
        for file in &evidence.files {
            revision
                .remove_relative_file(&payload_components(file.target.relative.as_str()))
                .map_err(|_| ImportError::Storage)?;
        }
        prune(&revision, evidence)?;
        // Windows deletes an object only when every open handle shares delete;
        // the pinned revision handle denies delete sharing, so it must go before
        // the bound `revisions/` handle removes the name.
        drop(revision);
        match self.tree.remove_revision_dir(&name) {
            Ok(true) => {}
            Ok(false) => return Err(ImportError::StageChanged),
            Err(_) => return Err(ImportError::Storage),
        }
        self.tree
            .sync_revisions()
            .map_err(|_| ImportError::Storage)?;
        self.tree.sync_root().map_err(|_| ImportError::Storage)
    }
}

fn copy_archive(
    revision: &fsops::StagedDirectory,
    input: &mut dyn Read,
) -> Result<[u8; 32], ImportError> {
    let mut file = revision
        .create_private_file(&[ARCHIVE_FILE])
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

fn extract(revision: &fsops::StagedDirectory) -> Result<Vec<PayloadFile>, ImportError> {
    let file = revision
        .open_relative_file(&[ARCHIVE_FILE])
        .map_err(|_| ImportError::StageUnavailable)?;
    let mut admitted = admit_zip(file, M1_ZIP_LIMITS).map_err(ImportError::Archive)?;

    let mut files = Vec::with_capacity(admitted.entries().len());
    for index in 0..admitted.entries().len() {
        let (relative, declared_len) = {
            let entry = &admitted.entries()[index];
            (entry.path.clone(), entry.uncompressed_len)
        };
        let file = revision
            .create_private_file(&payload_components(relative.as_str()))
            .map_err(|error| {
                // A duplicate path is a proved shape conflict. A refused or
                // exhausted filesystem leaves the stage unverified, not changed.
                if error.kind() == io::ErrorKind::AlreadyExists {
                    ImportError::StageChanged
                } else {
                    ImportError::StageUnavailable
                }
            })?;
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

fn flush(revision: &fsops::StagedDirectory, evidence: &StageEvidence) -> Result<(), ImportError> {
    sync_relative_file(revision, &[ARCHIVE_FILE])?;
    let mut directories = BTreeSet::new();
    directories.insert(Vec::<String>::new());
    for file in &evidence.files {
        sync_relative_file(revision, &payload_components(file.target.relative.as_str()))?;
        let mut directory = vec![PAYLOAD_DIR.to_owned()];
        directories.insert(directory.clone());
        let components = file.target.relative.as_str().split('/').collect::<Vec<_>>();
        for component in &components[..components.len().saturating_sub(1)] {
            directory.push((*component).to_owned());
            directories.insert(directory.clone());
        }
    }
    for directory in directories {
        let relative = directory.iter().map(String::as_str).collect::<Vec<_>>();
        revision
            .sync_relative_dir(&relative)
            .map_err(|_| ImportError::Storage)?;
    }
    Ok(())
}

fn sync_relative_file(
    revision: &fsops::StagedDirectory,
    components: &[&str],
) -> Result<(), ImportError> {
    revision
        .open_relative_file(components)
        .and_then(|file| file.sync_all())
        .map_err(|_| ImportError::Storage)
}

/// Only a proved digest, length or shape mismatch is `StageChanged`.
/// Permissions and transient I/O failures leave the stage unverified.
fn verify_tree(
    revision: &fsops::StagedDirectory,
    evidence: &StageEvidence,
) -> Result<(), ImportError> {
    if revision.identity() != &evidence.owner {
        return Err(ImportError::StageChanged);
    }

    let (archive_sha256, _) = hash_regular(revision, &[ARCHIVE_FILE])?;
    if archive_sha256 != evidence.archive_sha256 {
        return Err(ImportError::StageChanged);
    }
    let archive_file = revision
        .open_relative_file(&[ARCHIVE_FILE])
        .map_err(|_| ImportError::StageUnavailable)?;
    // A read failure is not an admission shape error: the snapshot stays
    // unverified rather than proved changed.
    admit_zip(archive_file, M1_ZIP_LIMITS).map_err(|error| match error {
        ArchiveError::Io => ImportError::StageUnavailable,
        _ => ImportError::StageChanged,
    })?;

    let mut top_level = BTreeSet::new();
    for (name, is_file, is_dir) in revision
        .list_relative(&[])
        .map_err(|_| ImportError::StageUnavailable)?
    {
        match (name.as_str(), is_file, is_dir) {
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
    if top_level.contains(PAYLOAD_DIR) {
        collect_files(revision, &mut vec![PAYLOAD_DIR.to_owned()], &mut found)?;
    } else if !evidence.files.is_empty() {
        return Err(ImportError::StageChanged);
    }

    let mut expected = BTreeSet::new();
    for file in &evidence.files {
        if !expected.insert(file.target.relative.as_str().to_owned()) {
            return Err(ImportError::StageChanged);
        }
        let (sha256, len) =
            hash_regular(revision, &payload_components(file.target.relative.as_str()))?;
        if sha256 != file.sha256 || len != file.len {
            return Err(ImportError::StageChanged);
        }
    }
    if found != expected {
        return Err(ImportError::StageChanged);
    }

    Ok(())
}

fn collect_files(
    revision: &fsops::StagedDirectory,
    components: &mut Vec<String>,
    found: &mut BTreeSet<String>,
) -> Result<(), ImportError> {
    let relative = components.iter().map(String::as_str).collect::<Vec<_>>();
    for (name, is_file, is_dir) in revision
        .list_relative(&relative)
        .map_err(|_| ImportError::StageUnavailable)?
    {
        components.push(name);
        if is_dir {
            collect_files(revision, components, found)?;
        } else if is_file {
            found.insert(components[1..].join("/"));
        } else {
            return Err(ImportError::StageChanged);
        }
        components.pop();
    }
    Ok(())
}

fn hash_regular(
    revision: &fsops::StagedDirectory,
    components: &[&str],
) -> Result<([u8; 32], u64), ImportError> {
    let mut file = revision
        .open_relative_file(components)
        .map_err(|error| match error.kind() {
            // A missing expected entry is a proved shape change. Permissions,
            // descriptor exhaustion and transient I/O leave the stage unverified.
            io::ErrorKind::NotFound => ImportError::StageChanged,
            _ => ImportError::StageUnavailable,
        })?;
    if !file
        .metadata()
        .map_err(|_| ImportError::StageUnavailable)?
        .is_file()
    {
        return Err(ImportError::StageChanged);
    }
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| ImportError::StageUnavailable)?;
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

fn prune(revision: &fsops::StagedDirectory, evidence: &StageEvidence) -> Result<(), ImportError> {
    let mut deep = vec![vec![PAYLOAD_DIR.to_owned()]];
    for file in &evidence.files {
        let mut path = vec![PAYLOAD_DIR.to_owned()];
        let components = file.target.relative.as_str().split('/').collect::<Vec<_>>();
        for component in &components[..components.len().saturating_sub(1)] {
            path.push((*component).to_owned());
            deep.push(path.clone());
        }
    }
    // Remove children before their parents so each unlink sees an empty
    // directory; a non-empty directory is a proved shape change.
    deep.sort_by_key(|path| std::cmp::Reverse(path.len()));
    deep.dedup();
    for path in deep {
        let relative = path.iter().map(String::as_str).collect::<Vec<_>>();
        match revision.remove_relative_dir(&relative) {
            Ok(true) => {}
            Ok(false) => return Err(ImportError::StageChanged),
            Err(_) => return Err(ImportError::Storage),
        }
    }
    Ok(())
}

fn payload_components(relative: &str) -> Vec<&str> {
    let mut components = Vec::with_capacity(relative.split('/').count() + 1);
    components.push(PAYLOAD_DIR);
    components.extend(relative.split('/'));
    components
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

    pub(crate) use crate::linux::{StagedDirectory, StagingTree};

    /// The private staging adapter may run where the record proves the staging
    /// layer; `marduk_core::qualification` decides. The import gate is a higher
    /// rung.
    pub fn qualified(root: &Path) -> bool {
        crate::native::library_volume_qualified(root)
    }

    pub fn bind(root: &Path) -> Result<StagingTree, RootError> {
        StagingTree::open(root)
    }

    /// A redirect or changed bound object is `StageChanged`; an unreachable
    /// tree is `StageUnavailable`.
    pub fn stage_error(error: RootError) -> ImportError {
        match error {
            RootError::TargetUnavailable | RootError::OutsideBoundary => ImportError::StageChanged,
            _ => ImportError::StageUnavailable,
        }
    }
}

#[cfg(windows)]
mod fsops {
    use super::*;

    pub(crate) use crate::windows::{StagedDirectory, StagingTree};

    /// The private staging adapter may run where the record proves the staging
    /// layer (NTFS today); `marduk_core::qualification` decides, not this seam.
    /// The product import gate is a higher rung and stays closed.
    pub fn qualified(root: &Path) -> bool {
        crate::windows::staging_volume_qualified(root)
    }

    pub fn bind(root: &Path) -> Result<StagingTree, RootError> {
        StagingTree::open(root)
    }

    /// A redirect or changed bound object is `StageChanged`; an unreachable
    /// tree is `StageUnavailable`.
    pub fn stage_error(error: RootError) -> ImportError {
        match error {
            RootError::TargetUnavailable | RootError::OutsideBoundary => ImportError::StageChanged,
            _ => ImportError::StageUnavailable,
        }
    }
}
