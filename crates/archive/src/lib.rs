//! Bounded classic-ZIP admission and streaming. No extraction to caller paths.

pub use marduk_core::import::{ArchiveError, LimitKind};

mod zip;
pub use zip::{AdmittedZip, M1_ZIP_LIMITS, ZipEntry, ZipLimits, admit_zip};
