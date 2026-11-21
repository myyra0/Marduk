//! Shared import and archive vocabulary. Storage adds the import protocol.

use std::collections::BTreeMap;

use crate::ids::{InstalledRevisionId, MemberId, ProfileId, ProfileRevision};
use crate::profiles::ProfileSnapshot;
use crate::root_identity::RootIdentity;
use crate::targets::TargetFile;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitKind {
    ArchiveBytes,
    DirectoryBytes,
    Entries,
    FileBytes,
    TotalBytes,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArchiveError {
    InvalidContainer,
    UnsupportedContainer,
    DuplicateRawName,
    UnsupportedEntry { index: usize },
    UnsafeName { index: usize },
    ConflictingNames { first: usize, second: usize },
    LimitExceeded { kind: LimitKind },
    CorruptEntry { index: usize },
    MissingEntry { index: usize },
    Io,
}

/// Opaque catalog allocation. The value must round-trip through SQLite as a
/// positive signed 64-bit integer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ImportId(u64);

impl ImportId {
    pub fn try_from_u64(value: u64) -> Result<Self, ImportError> {
        if value == 0 || value > i64::MAX as u64 {
            return Err(ImportError::Storage);
        }
        Ok(Self(value))
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug)]
pub struct BeginImport {
    pub profile: ProfileId,
    pub expected_revision: ProfileRevision,
    pub target: ImportTarget,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportTarget {
    Append { display_name: String },
    Replace { member: MemberId },
}

#[derive(Clone, Debug)]
pub struct PreparedImport {
    pub id: ImportId,
    pub profile: ProfileId,
    pub expected_revision: ProfileRevision,
    pub archive_sha256: [u8; 32],
    pub files: Vec<TargetFile>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportError {
    InvalidName,
    StalePlan,
    MemberNotFound,
    Busy,
    UnknownImport,
    ImportNotPrepared,
    StageChanged,
    StageUnavailable,
    Archive(ArchiveError),
    Storage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileEditError {
    StalePlan,
    InvalidOrder,
    Busy,
    Storage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImportAllocation {
    pub id: ImportId,
    pub revision: InstalledRevisionId,
}

#[derive(Clone, Debug)]
pub struct PayloadFile {
    pub target: TargetFile,
    pub len: u64,
    pub sha256: [u8; 32],
}

#[derive(Clone, Debug)]
pub struct StageEvidence {
    pub allocation: ImportAllocation,
    pub archive_sha256: [u8; 32],
    pub files: Vec<PayloadFile>,
    pub owner: RootIdentity,
}

/// Profile snapshot joined with display names from the same read transaction.
#[derive(Clone, Debug)]
pub struct NamedProfile {
    pub snapshot: ProfileSnapshot,
    pub names: BTreeMap<MemberId, String>,
}
