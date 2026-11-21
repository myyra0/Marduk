use std::io::{Read, Seek, Write};

use marduk_core::import::ArchiveError;
use marduk_core::targets::RelativePath;

#[derive(Clone, Copy, Debug)]
pub struct ZipLimits {
    pub max_archive_bytes: u64,
    pub max_directory_bytes: u64,
    pub max_entries: usize,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
}

pub const M1_ZIP_LIMITS: ZipLimits = ZipLimits {
    max_archive_bytes: 5 << 30,
    max_directory_bytes: 16 << 20,
    max_entries: 10_000,
    max_file_bytes: 1 << 30,
    max_total_bytes: 4 << 30,
};

#[derive(Clone, Debug)]
pub struct ZipEntry {
    pub path: RelativePath,
    pub uncompressed_len: u64,
}

pub struct AdmittedZip<R> {
    _input: std::marker::PhantomData<R>,
}

pub fn admit_zip<R: Read + Seek>(
    input: R,
    limits: ZipLimits,
) -> Result<AdmittedZip<R>, ArchiveError> {
    let _ = (input, limits);
    Err(ArchiveError::Io)
}

impl<R: Read + Seek> AdmittedZip<R> {
    pub fn entries(&self) -> &[ZipEntry] {
        &[]
    }

    pub fn copy_entry(&mut self, index: usize, out: &mut impl Write) -> Result<u64, ArchiveError> {
        let _ = (index, out);
        Err(ArchiveError::Io)
    }
}
