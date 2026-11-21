//! Bounded classic-ZIP admission.
//!
//! Admission proves the container is a single-disk ZIP32 with a coherent central
//! directory before the library reader allocates or indexes anything. Only after
//! that proof is `ZipArchive` constructed and its public metadata cross-checked
//! against the bounded descriptors.

use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Seek, SeekFrom, Write};

use marduk_core::import::{ArchiveError, LimitKind};
use marduk_core::targets::RelativePath;
use zip::result::ZipError;
use zip::{CompressionMethod, ZipArchive};

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
    reader: ZipArchive<R>,
    entries: Vec<ZipEntry>,
    /// Central-directory position for each manifest entry, in manifest order.
    central_indices: Vec<usize>,
    limits: ZipLimits,
}

pub fn admit_zip<R: Read + Seek>(
    mut input: R,
    limits: ZipLimits,
) -> Result<AdmittedZip<R>, ArchiveError> {
    let archive_len = input.seek(SeekFrom::End(0)).map_err(source_error)?;
    if archive_len > limits.max_archive_bytes {
        return Err(limit(LimitKind::ArchiveBytes));
    }

    let end = read_end_of_central_directory(&mut input, archive_len)?;

    // Limits are checked from the EOCD alone, before any directory index.
    if end.directory_size > limits.max_directory_bytes {
        return Err(limit(LimitKind::DirectoryBytes));
    }
    if end.count > limits.max_entries {
        return Err(limit(LimitKind::Entries));
    }

    if end
        .directory_offset
        .checked_add(end.directory_size)
        .filter(|end_of_directory| *end_of_directory == end.start)
        .is_none()
    {
        return Err(ArchiveError::InvalidContainer);
    }
    let minimum_directory_size = u64::try_from(end.count)
        .ok()
        .and_then(|count| count.checked_mul(CENTRAL_FIXED))
        .ok_or(ArchiveError::InvalidContainer)?;
    if minimum_directory_size > end.directory_size {
        return Err(ArchiveError::InvalidContainer);
    }
    if end.count == 0 && (end.directory_size != 0 || end.directory_offset != 0) {
        return Err(ArchiveError::InvalidContainer);
    }

    let directory = read_directory(&mut input, &end)?;
    let mut records = parse_directory(&directory, &end)?;
    verify_local_extents(&mut input, &records, end.directory_offset)?;

    validate_names(&mut records)?;
    validate_duplicate_names(&records)?;
    validate_conflicts(&records)?;
    validate_metadata(&records)?;
    validate_expansion(&records, &limits)?;

    let entries = records
        .iter()
        .filter(|record| !record.is_dir)
        .map(|record| ZipEntry {
            path: record
                .path
                .clone()
                .expect("names validated before the manifest is built"),
            uncompressed_len: u64::from(record.expanded),
        })
        .collect();
    let central_indices = records
        .iter()
        .filter(|record| !record.is_dir)
        .map(|record| record.index)
        .collect();

    let mut reader = ZipArchive::new(input).map_err(container_error)?;
    verify_reader(&mut reader, &records, &end)?;
    drain_directories(&mut reader, &records)?;

    Ok(AdmittedZip {
        reader,
        entries,
        central_indices,
        limits,
    })
}

impl<R: Read + Seek> AdmittedZip<R> {
    pub fn entries(&self) -> &[ZipEntry] {
        &self.entries
    }

    pub fn copy_entry(&mut self, index: usize, out: &mut impl Write) -> Result<u64, ArchiveError> {
        let (central_index, declared_len) = self
            .central_indices
            .get(index)
            .zip(self.entries.get(index))
            .map(|(&central_index, entry)| (central_index, entry.uncompressed_len))
            .ok_or(ArchiveError::MissingEntry { index })?;
        if declared_len > self.limits.max_file_bytes {
            return Err(limit(LimitKind::FileBytes));
        }

        let mut file = self
            .reader
            .by_index(central_index)
            .map_err(|error| entry_open_error(error, index))?;
        let mut buffer = [0u8; 64 * 1024];
        let mut written: u64 = 0;
        loop {
            // Read through EOF, never `.take(declared_len)`: the reader must finish
            // its own checksum validation.
            let read = file
                .read(&mut buffer)
                .map_err(|error| read_error(error, index))?;
            if read == 0 {
                break;
            }
            written = written
                .checked_add(read as u64)
                .ok_or(ArchiveError::CorruptEntry { index })?;
            if written > declared_len {
                return Err(ArchiveError::CorruptEntry { index });
            }
            out.write_all(&buffer[..read])
                .map_err(|_| ArchiveError::Io)?;
        }
        if written != declared_len {
            return Err(ArchiveError::CorruptEntry { index });
        }
        Ok(written)
    }
}

const EOCD_SIGNATURE: u32 = 0x0605_4b50;
const ZIP64_LOCATOR_SIGNATURE: u32 = 0x0706_4b50;
const CENTRAL_SIGNATURE: u32 = 0x0201_4b50;
const LOCAL_SIGNATURE: u32 = 0x0403_4b50;

/// Fixed EOCD size plus the classic 64 KiB comment window.
const EOCD_TAIL: u64 = 22 + 65_536;
const EOCD_FIXED: usize = 22;
const CENTRAL_FIXED: u64 = 46;
const LOCAL_FIXED: u64 = 30;

const ZIP64_EXTRA_ID: u16 = 0x0001;
const AES_EXTRA_ID: u16 = 0x9901;
const UNICODE_PATH_EXTRA_ID: u16 = 0x7075;

/// Encrypted, patched, strong-encryption and masked-header flags.
const UNSUPPORTED_FLAGS: u16 = 0x0001 | 0x0020 | 0x0040 | 0x2000;
/// The reader may set this when an agreeing Unicode-path extra field is present.
const LANGUAGE_ENCODING_FLAG: u16 = 0x0800;

const UNIX_TYPE_MASK: u32 = 0o170000;
const UNIX_DIRECTORY: u32 = 0o040000;
const UNIX_REGULAR: u32 = 0o100000;
const DOS_DIRECTORY: u32 = 0x10;

struct EndOfCentralDirectory {
    /// Absolute offset of the EOCD record.
    start: u64,
    directory_offset: u64,
    directory_size: u64,
    count: usize,
}

struct Record {
    /// Central-directory position, also the reader index once duplicates are excluded.
    index: usize,
    central_start: u64,
    raw_name: Vec<u8>,
    flags: u16,
    method: u16,
    crc: u32,
    compressed: u32,
    expanded: u32,
    external: u32,
    local_offset: u32,
    name_len: u16,
    has_aes_extra: bool,
    spelling: String,
    is_dir: bool,
    path: Option<RelativePath>,
}

fn read_end_of_central_directory<R: Read + Seek>(
    input: &mut R,
    archive_len: u64,
) -> Result<EndOfCentralDirectory, ArchiveError> {
    if archive_len < EOCD_FIXED as u64 {
        return Err(ArchiveError::InvalidContainer);
    }
    let tail_len = archive_len.min(EOCD_TAIL);
    let tail_start = archive_len - tail_len;
    input
        .seek(SeekFrom::Start(tail_start))
        .map_err(source_error)?;
    let mut tail =
        vec![0u8; usize::try_from(tail_len).map_err(|_| ArchiveError::InvalidContainer)?];
    input.read_exact(&mut tail).map_err(source_error)?;

    // Classic ZIP has exactly one EOCD. Any second signature in the bounded window
    // could point the reader at an alternate directory, so refuse the ambiguity.
    let signature = EOCD_SIGNATURE.to_le_bytes();
    let mut accepted = None;
    for (at, window) in tail.windows(signature.len()).enumerate() {
        if window != signature {
            continue;
        }
        if accepted.is_some() {
            return Err(ArchiveError::UnsupportedContainer);
        }
        accepted = Some(at);
    }
    let Some(offset) = accepted else {
        return Err(ArchiveError::InvalidContainer);
    };
    if offset + EOCD_FIXED > tail.len() {
        return Err(ArchiveError::UnsupportedContainer);
    }

    let block = &tail[offset..offset + EOCD_FIXED];
    let comment_len = usize::from(u16_at(block, 20));
    let end = offset + EOCD_FIXED + comment_len;
    if end > tail.len() {
        return Err(ArchiveError::InvalidContainer);
    }
    if end < tail.len() {
        return Err(ArchiveError::UnsupportedContainer);
    }

    let disk_number = u16_at(block, 4);
    let directory_disk = u16_at(block, 6);
    let files_on_disk = u16_at(block, 8);
    let files = u16_at(block, 10);
    let directory_size = u32_at(block, 12);
    let directory_offset = u32_at(block, 16);

    if disk_number == u16::MAX
        || directory_disk == u16::MAX
        || files_on_disk == u16::MAX
        || files == u16::MAX
        || directory_size == u32::MAX
        || directory_offset == u32::MAX
    {
        return Err(ArchiveError::UnsupportedContainer);
    }
    if offset >= 20 && tail[offset - 20..offset - 16] == ZIP64_LOCATOR_SIGNATURE.to_le_bytes() {
        return Err(ArchiveError::UnsupportedContainer);
    }
    if disk_number != 0 || directory_disk != 0 || files_on_disk != files {
        return Err(ArchiveError::UnsupportedContainer);
    }

    Ok(EndOfCentralDirectory {
        start: tail_start + offset as u64,
        directory_offset: u64::from(directory_offset),
        directory_size: u64::from(directory_size),
        count: usize::from(files),
    })
}

fn read_directory<R: Read + Seek>(
    input: &mut R,
    end: &EndOfCentralDirectory,
) -> Result<Vec<u8>, ArchiveError> {
    let len = usize::try_from(end.directory_size).map_err(|_| ArchiveError::InvalidContainer)?;
    let mut directory = vec![0u8; len];
    input
        .seek(SeekFrom::Start(end.directory_offset))
        .map_err(source_error)?;
    input.read_exact(&mut directory).map_err(source_error)?;
    Ok(directory)
}

fn parse_directory(
    directory: &[u8],
    end: &EndOfCentralDirectory,
) -> Result<Vec<Record>, ArchiveError> {
    let mut records = Vec::with_capacity(end.count);
    let mut cursor = 0usize;
    for index in 0..end.count {
        let fixed_end = cursor
            .checked_add(CENTRAL_FIXED as usize)
            .filter(|fixed_end| *fixed_end <= directory.len())
            .ok_or(ArchiveError::InvalidContainer)?;
        let fixed = &directory[cursor..fixed_end];
        if u32_at(fixed, 0) != CENTRAL_SIGNATURE {
            return Err(ArchiveError::InvalidContainer);
        }

        let flags = u16_at(fixed, 8);
        let method = u16_at(fixed, 10);
        let crc = u32_at(fixed, 16);
        let compressed = u32_at(fixed, 20);
        let expanded = u32_at(fixed, 24);
        let name_len = u16_at(fixed, 28);
        let extra_len = u16_at(fixed, 30);
        let comment_len = u16_at(fixed, 32);
        let disk_start = u16_at(fixed, 34);
        let external = u32_at(fixed, 38);
        let local_offset = u32_at(fixed, 42);

        if disk_start != 0 {
            return Err(ArchiveError::UnsupportedContainer);
        }
        if compressed == u32::MAX || expanded == u32::MAX || local_offset == u32::MAX {
            return Err(ArchiveError::UnsupportedContainer);
        }

        let name_start = fixed_end;
        let name_end = name_start
            .checked_add(usize::from(name_len))
            .ok_or(ArchiveError::InvalidContainer)?;
        let extra_end = name_end
            .checked_add(usize::from(extra_len))
            .ok_or(ArchiveError::InvalidContainer)?;
        let record_end = extra_end
            .checked_add(usize::from(comment_len))
            .filter(|record_end| *record_end <= directory.len())
            .ok_or(ArchiveError::InvalidContainer)?;

        let raw_name = directory[name_start..name_end].to_vec();
        let has_aes_extra = parse_extra_fields(&directory[name_end..extra_end], &raw_name, index)?;

        records.push(Record {
            index,
            central_start: end.directory_offset + cursor as u64,
            raw_name,
            flags,
            method,
            crc,
            compressed,
            expanded,
            external,
            local_offset,
            name_len,
            has_aes_extra,
            spelling: String::new(),
            is_dir: false,
            path: None,
        });
        cursor = record_end;
    }
    if cursor != directory.len() {
        return Err(ArchiveError::InvalidContainer);
    }

    let mut offsets = HashSet::with_capacity(records.len());
    for record in &records {
        if !offsets.insert(record.local_offset) {
            return Err(ArchiveError::InvalidContainer);
        }
    }
    // Prepended bytes shift every local header even when the EOCD agrees.
    if !records.is_empty() && !offsets.contains(&0) {
        return Err(ArchiveError::UnsupportedContainer);
    }

    Ok(records)
}

/// Bound every local header and its data before the reader is constructed.
///
/// The local header, not the central record, decides where entry data starts, so
/// the extent is computed from the local name/extra lengths. The central name and
/// compressed size still bound it as a redundant minimum.
fn verify_local_extents<R: Read + Seek>(
    input: &mut R,
    records: &[Record],
    directory_offset: u64,
) -> Result<(), ArchiveError> {
    let mut header = [0u8; LOCAL_FIXED as usize];
    for record in records {
        let start = u64::from(record.local_offset);
        let header_end = start
            .checked_add(LOCAL_FIXED)
            .filter(|header_end| *header_end <= directory_offset)
            .ok_or(ArchiveError::InvalidContainer)?;
        input.seek(SeekFrom::Start(start)).map_err(source_error)?;
        input.read_exact(&mut header).map_err(source_error)?;
        if u32_at(&header, 0) != LOCAL_SIGNATURE {
            return Err(ArchiveError::InvalidContainer);
        }
        if u32_at(&header, 18) == u32::MAX || u32_at(&header, 22) == u32::MAX {
            return Err(ArchiveError::UnsupportedContainer);
        }

        let name_len = usize::from(u16_at(&header, 26));
        let extra_len = usize::from(u16_at(&header, 28));
        let variable_end = header_end
            .checked_add(name_len as u64)
            .and_then(|variable_end| variable_end.checked_add(extra_len as u64))
            .filter(|variable_end| *variable_end <= directory_offset)
            .ok_or(ArchiveError::InvalidContainer)?;

        let mut variables = vec![0u8; name_len + extra_len];
        input.read_exact(&mut variables).map_err(source_error)?;
        if variables[..name_len] != record.raw_name[..] {
            return Err(ArchiveError::InvalidContainer);
        }
        parse_extra_fields(&variables[name_len..], &record.raw_name, record.index)?;

        let extent = variable_end
            .checked_add(u64::from(record.compressed))
            .ok_or(ArchiveError::InvalidContainer)?;
        if extent > directory_offset {
            return Err(ArchiveError::InvalidContainer);
        }

        // Redundant lower bound from the central record. A larger central extra
        // field is legal because it does not move the local data start.
        let minimum = start
            .checked_add(LOCAL_FIXED)
            .and_then(|minimum| minimum.checked_add(u64::from(record.name_len)))
            .and_then(|minimum| minimum.checked_add(u64::from(record.compressed)))
            .ok_or(ArchiveError::InvalidContainer)?;
        if minimum > directory_offset {
            return Err(ArchiveError::InvalidContainer);
        }
    }
    Ok(())
}

fn parse_extra_fields(extra: &[u8], raw_name: &[u8], index: usize) -> Result<bool, ArchiveError> {
    let mut has_aes = false;
    let mut cursor = 0usize;
    while cursor < extra.len() {
        let header_end = cursor
            .checked_add(4)
            .filter(|header_end| *header_end <= extra.len())
            .ok_or(ArchiveError::InvalidContainer)?;
        let id = u16_at(extra, cursor);
        let payload_start = header_end;
        let payload_end = payload_start
            .checked_add(usize::from(u16_at(extra, cursor + 2)))
            .filter(|payload_end| *payload_end <= extra.len())
            .ok_or(ArchiveError::InvalidContainer)?;
        match id {
            ZIP64_EXTRA_ID => return Err(ArchiveError::UnsupportedContainer),
            AES_EXTRA_ID => has_aes = true,
            UNICODE_PATH_EXTRA_ID
                if unicode_path_rewrites(&extra[payload_start..payload_end], raw_name) =>
            {
                return Err(ArchiveError::UnsafeName { index });
            }
            _ => {}
        }
        cursor = payload_end;
    }
    Ok(has_aes)
}

fn unicode_path_rewrites(extra: &[u8], raw_name: &[u8]) -> bool {
    if extra.len() < 5 || extra[0] != 1 {
        return false;
    }
    let crc = u32::from_le_bytes([extra[1], extra[2], extra[3], extra[4]]);
    if crc != crc32(raw_name) {
        return false;
    }
    let content = &extra[5..];
    content != raw_name
}

/// Validation pass 1: strict raw UTF-8 and C01 relative-path rules.
fn validate_names(records: &mut [Record]) -> Result<(), ArchiveError> {
    for record in records.iter_mut() {
        let index = record.index;
        let name = std::str::from_utf8(&record.raw_name)
            .map_err(|_| ArchiveError::UnsafeName { index })?;
        let (spelling, is_dir) = match name.strip_suffix('/') {
            Some(spelling) => (spelling, true),
            None => (name, false),
        };
        let path = RelativePath::parse(spelling).map_err(|_| ArchiveError::UnsafeName { index })?;
        record.spelling = spelling.to_owned();
        record.is_dir = is_dir;
        record.path = Some(path);
    }
    Ok(())
}

/// Validation pass 2a: an exact raw-name duplicate is never a last-wins entry.
fn validate_duplicate_names(records: &[Record]) -> Result<(), ArchiveError> {
    let mut seen: HashSet<&[u8]> = HashSet::with_capacity(records.len());
    for record in records {
        if !seen.insert(record.raw_name.as_slice()) {
            return Err(ArchiveError::DuplicateRawName);
        }
    }
    Ok(())
}

/// Validation pass 2b: ASCII-folded equivalence, mixed parent spellings and
/// file/directory overlaps. Conflicts report the earliest earlier witness.
fn validate_conflicts(records: &[Record]) -> Result<(), ArchiveError> {
    struct Claim {
        spelling: String,
        first: usize,
        is_dir: bool,
    }

    let mut claims: HashMap<String, Claim> = HashMap::with_capacity(records.len());
    for record in records {
        for (at, _) in record.spelling.match_indices('/') {
            let prefix = &record.spelling[..at];
            let canonical = prefix.to_ascii_lowercase();
            let existing = claims
                .get(&canonical)
                .map(|claim| (claim.first, claim.is_dir, claim.spelling != prefix));
            match existing {
                Some((first, is_dir, differs_spelling)) if !is_dir || differs_spelling => {
                    return Err(ArchiveError::ConflictingNames {
                        first,
                        second: record.index,
                    });
                }
                Some(_) => {}
                None => {
                    claims.insert(
                        canonical,
                        Claim {
                            spelling: prefix.to_owned(),
                            first: record.index,
                            is_dir: true,
                        },
                    );
                }
            }
        }

        let canonical = record.spelling.to_ascii_lowercase();
        let existing = claims.get(&canonical).map(|claim| {
            (
                claim.first,
                claim.spelling != record.spelling || claim.is_dir != record.is_dir,
            )
        });
        match existing {
            Some((first, true)) => {
                return Err(ArchiveError::ConflictingNames {
                    first,
                    second: record.index,
                });
            }
            Some(_) => {}
            None => {
                claims.insert(
                    canonical,
                    Claim {
                        spelling: record.spelling.clone(),
                        first: record.index,
                        is_dir: record.is_dir,
                    },
                );
            }
        }
    }
    Ok(())
}

/// Validation pass 3: encryption, special modes, marker consistency and methods.
fn validate_metadata(records: &[Record]) -> Result<(), ArchiveError> {
    for record in records {
        let unsupported = ArchiveError::UnsupportedEntry {
            index: record.index,
        };
        if record.flags & UNSUPPORTED_FLAGS != 0
            || record.has_aes_extra
            || record.method == 99
            || (record.method != 0 && record.method != 8)
        {
            return Err(unsupported);
        }

        let unix_type = (record.external >> 16) & UNIX_TYPE_MASK;
        if unix_type != 0 && unix_type != UNIX_REGULAR && unix_type != UNIX_DIRECTORY {
            return Err(unsupported);
        }
        if unix_type != 0 {
            if (unix_type == UNIX_DIRECTORY) != record.is_dir {
                return Err(unsupported);
            }
        } else if record.external & DOS_DIRECTORY != 0 && !record.is_dir {
            return Err(unsupported);
        }

        // Explicit directories are metadata only: empty content and empty CRC.
        if record.is_dir && (record.expanded != 0 || record.crc != 0) {
            return Err(unsupported);
        }
    }
    Ok(())
}

/// Validation pass 4: per-file then checked total declared expanded caps.
fn validate_expansion(records: &[Record], limits: &ZipLimits) -> Result<(), ArchiveError> {
    let mut total: u64 = 0;
    for record in records.iter().filter(|record| !record.is_dir) {
        let len = u64::from(record.expanded);
        if len > limits.max_file_bytes {
            return Err(limit(LimitKind::FileBytes));
        }
        total = total
            .checked_add(len)
            .ok_or_else(|| limit(LimitKind::TotalBytes))?;
        if total > limits.max_total_bytes {
            return Err(limit(LimitKind::TotalBytes));
        }
    }
    Ok(())
}

fn verify_reader<R: Read + Seek>(
    reader: &mut ZipArchive<R>,
    records: &[Record],
    end: &EndOfCentralDirectory,
) -> Result<(), ArchiveError> {
    if reader.has_duplicate_names() {
        return Err(ArchiveError::DuplicateRawName);
    }
    if reader.len() != records.len()
        || reader.central_directory_start() != end.directory_offset
        || reader.offset() != 0
    {
        return Err(ArchiveError::InvalidContainer);
    }
    reader
        .validate_header_consistency()
        .map_err(container_error)?;

    for record in records {
        let entry = reader
            .by_index_data(record.index)
            .map_err(container_error)?;
        if entry.name_raw() != record.raw_name.as_slice() {
            return Err(ArchiveError::UnsafeName {
                index: record.index,
            });
        }
        let method_matches = match entry.compression() {
            CompressionMethod::Stored => record.method == 0,
            CompressionMethod::Deflated => record.method == 8,
            _ => false,
        };
        if (entry.flags().as_u16() ^ record.flags) & !LANGUAGE_ENCODING_FLAG != 0
            || !method_matches
            || entry.size() != u64::from(record.expanded)
            || entry.compressed_size() != u64::from(record.compressed)
            || entry.crc32() != record.crc
            || entry.external_attributes() != record.external
            || entry.header_start() != u64::from(record.local_offset)
            || entry.central_header_start() != record.central_start
            || entry.is_dir() != record.is_dir
            || entry.is_symlink()
        {
            return Err(ArchiveError::InvalidContainer);
        }
    }
    Ok(())
}

/// Drain explicit directories against their zero-length declaration so corruption
/// is reported at admission, with the central-directory index.
fn drain_directories<R: Read + Seek>(
    reader: &mut ZipArchive<R>,
    records: &[Record],
) -> Result<(), ArchiveError> {
    let mut buffer = [0u8; 1024];
    for record in records.iter().filter(|record| record.is_dir) {
        let mut file = reader
            .by_index(record.index)
            .map_err(|error| entry_open_error(error, record.index))?;
        loop {
            match file.read(&mut buffer) {
                Ok(0) => break,
                Ok(_) => {
                    return Err(ArchiveError::CorruptEntry {
                        index: record.index,
                    });
                }
                Err(error) => return Err(read_error(error, record.index)),
            }
        }
    }
    Ok(())
}

fn limit(kind: LimitKind) -> ArchiveError {
    ArchiveError::LimitExceeded { kind }
}

fn source_error(error: io::Error) -> ArchiveError {
    if error.kind() == io::ErrorKind::UnexpectedEof {
        ArchiveError::InvalidContainer
    } else {
        ArchiveError::Io
    }
}

fn container_error(error: ZipError) -> ArchiveError {
    match error {
        ZipError::Io(inner) => source_error(inner),
        ZipError::UnsupportedArchive(_) => ArchiveError::UnsupportedContainer,
        _ => ArchiveError::InvalidContainer,
    }
}

fn entry_open_error(error: ZipError, index: usize) -> ArchiveError {
    match error {
        ZipError::Io(inner) if inner.kind() != io::ErrorKind::UnexpectedEof => ArchiveError::Io,
        _ => ArchiveError::CorruptEntry { index },
    }
}

/// Entry reads are integrity checks unless the source itself fails.
fn read_error(error: io::Error, index: usize) -> ArchiveError {
    match error.kind() {
        io::ErrorKind::UnexpectedEof | io::ErrorKind::InvalidData => {
            ArchiveError::CorruptEntry { index }
        }
        _ => ArchiveError::Io,
    }
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}
