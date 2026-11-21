//! Shared import and archive vocabulary. Storage adds the import protocol.

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
