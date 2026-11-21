//! Regression coverage for local-header extents and source read errors.
//!
//! The frozen admission oracle lives in `tests/admission.rs`; this file only adds
//! cases for the local header boundary and streaming error taxonomy.

#[allow(dead_code)]
mod fixtures;

use std::io::{self, Cursor, Read, Seek, SeekFrom};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use fixtures::{ClassicZip, Record};
use marduk_archive::{ArchiveError, M1_ZIP_LIMITS, admit_zip};

fn refused(bytes: Vec<u8>) -> Result<(), ArchiveError> {
    admit_zip(Cursor::new(bytes), M1_ZIP_LIMITS).map(|_| ())
}

/// Give the single central record an extra field the local header does not have.
fn append_central_extra(zip: &mut ClassicZip, extra: &[u8]) {
    let directory = zip.directory_start;
    let name_len = usize::from(u16::from_le_bytes([
        zip.bytes[directory + 28],
        zip.bytes[directory + 29],
    ]));
    let insert_at = directory + 46 + name_len;
    zip.bytes
        .splice(insert_at..insert_at, extra.iter().copied());
    zip.bytes[directory + 30..directory + 32]
        .copy_from_slice(&u16::try_from(extra.len()).unwrap().to_le_bytes());
    let directory_size = u32::try_from(zip.bytes.len() - 22 - directory).unwrap();
    zip.set_eocd(12, &directory_size.to_le_bytes());
}

#[test]
fn oversized_local_extra_crossing_the_directory_is_invalid() {
    // The file's four data bytes are a complete unknown extra field (ID 0xFFFF,
    // zero payload). Claiming them as the local extra field moves the data start to
    // the directory, so the declared data extent crosses `cd_start`.
    let mut zip = ClassicZip::new(&[Record::file(b"valid", b"\xff\xff\x00\x00")]);
    zip.bytes[28..30].copy_from_slice(&4u16.to_le_bytes());

    assert_eq!(refused(zip.bytes), Err(ArchiveError::InvalidContainer));
}

#[test]
fn larger_central_extra_still_admits_and_streams_the_last_entry() {
    let mut zip = ClassicZip::new(&[Record::file(b"last", b"abc")]);
    // Unknown extra field (ID 0xFFFF, two payload bytes), central record only.
    append_central_extra(&mut zip, &[0xff, 0xff, 0x02, 0x00, 0x00, 0x00]);

    let mut archive = admit_zip(Cursor::new(zip.bytes), M1_ZIP_LIMITS).unwrap();
    assert_eq!(archive.entries().len(), 1);
    let mut out = Vec::new();
    assert_eq!(archive.copy_entry(0, &mut out), Ok(3));
    assert_eq!(out, b"abc");
}

struct FailingSource {
    input: Cursor<Vec<u8>>,
    fail: Arc<AtomicBool>,
}

impl Read for FailingSource {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "injected source failure",
            ));
        }
        self.input.read(out)
    }
}

impl Seek for FailingSource {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.input.seek(position)
    }
}

#[test]
fn source_read_failure_while_streaming_is_io() {
    let zip = ClassicZip::new(&[Record::file(b"Data/item", b"payload")]);
    let fail = Arc::new(AtomicBool::new(false));
    let source = FailingSource {
        input: Cursor::new(zip.bytes),
        fail: Arc::clone(&fail),
    };
    let mut archive = admit_zip(source, M1_ZIP_LIMITS).unwrap();

    // Admission cached each local data start, so `by_index` needs no source read;
    // the failure happens while the entry payload is streamed.
    fail.store(true, Ordering::SeqCst);
    let mut out = Vec::new();
    assert_eq!(archive.copy_entry(0, &mut out), Err(ArchiveError::Io));
    assert!(out.is_empty());
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

/// Info-ZIP Unicode path extra field (0x7075) for the raw name's CRC.
fn unicode_path_extra(raw_name: &[u8], content: &[u8]) -> Vec<u8> {
    let mut extra = vec![0x75, 0x70, 0, 0, 1];
    extra.extend_from_slice(&crc32(raw_name).to_le_bytes());
    extra.extend_from_slice(content);
    let payload_len = u16::try_from(extra.len() - 4).unwrap();
    extra[2..4].copy_from_slice(&payload_len.to_le_bytes());
    extra
}

/// Mirror an extra field into the single local header, rebasing later offsets.
fn add_local_extra(zip: &mut ClassicZip, extra: &[u8]) {
    let name_len = usize::from(u16::from_le_bytes([zip.bytes[26], zip.bytes[27]]));
    let insert_at = 30 + name_len;
    zip.bytes
        .splice(insert_at..insert_at, extra.iter().copied());
    zip.bytes[28..30].copy_from_slice(&u16::try_from(extra.len()).unwrap().to_le_bytes());

    let shift = u32::try_from(extra.len()).unwrap();
    let eocd = zip.bytes.len() - 22;
    let directory_offset = u32::from_le_bytes(zip.bytes[eocd + 16..eocd + 20].try_into().unwrap());
    zip.bytes[eocd + 16..eocd + 20].copy_from_slice(&(directory_offset + shift).to_le_bytes());
    zip.directory_start += extra.len();
}

#[test]
fn unicode_path_rewrite_with_invalid_utf8_is_unsafe() {
    let raw_name = b"safe";
    let mut zip = ClassicZip::new(&[Record::file(raw_name, b"x")]);
    append_central_extra(&mut zip, &unicode_path_extra(raw_name, b"\xff\xfe"));

    assert_eq!(
        refused(zip.bytes),
        Err(ArchiveError::UnsafeName { index: 0 })
    );
}

#[test]
fn unicode_path_rewrite_matching_the_raw_name_is_admitted() {
    let raw_name = b"safe";
    let extra = unicode_path_extra(raw_name, raw_name);
    let mut zip = ClassicZip::new(&[Record::file(raw_name, b"x")]);
    add_local_extra(&mut zip, &extra);
    append_central_extra(&mut zip, &extra);

    let mut archive = admit_zip(Cursor::new(zip.bytes), M1_ZIP_LIMITS).unwrap();
    assert_eq!(archive.entries().len(), 1);
    assert_eq!(archive.entries()[0].path.as_str(), "safe");
    let mut out = Vec::new();
    assert_eq!(archive.copy_entry(0, &mut out), Ok(1));
    assert_eq!(out, b"x");
}

#[test]
fn second_end_of_central_directory_is_unsupported() {
    let mut zip = ClassicZip::new(&[Record::file(b"one", b"1")]);
    let eocd = zip.bytes[zip.bytes.len() - 22..].to_vec();
    zip.bytes.extend_from_slice(&eocd);

    assert_eq!(refused(zip.bytes), Err(ArchiveError::UnsupportedContainer));
}

struct PositionalFailure {
    input: Cursor<Vec<u8>>,
    fail_at: u64,
}

impl Read for PositionalFailure {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if !out.is_empty() && self.input.position() == self.fail_at {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "injected source failure",
            ));
        }
        self.input.read(out)
    }
}

impl Seek for PositionalFailure {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.input.seek(position)
    }
}

#[test]
fn source_read_failure_while_draining_a_directory_is_io() {
    // An empty DEFLATE stream declared as a directory: zero expanded bytes and
    // CRC, but compressed bytes that admission still drains.
    let directory = Record {
        method: 8,
        data: b"\x03\x00",
        ..Record::dir(b"Data/")
    };
    let zip = ClassicZip::new(&[directory]);
    let data_start = 35; // 30-byte local header + 5-byte name
    let source = PositionalFailure {
        input: Cursor::new(zip.bytes),
        fail_at: data_start,
    };

    assert_eq!(
        admit_zip(source, M1_ZIP_LIMITS).map(|_| ()),
        Err(ArchiveError::Io)
    );
}
