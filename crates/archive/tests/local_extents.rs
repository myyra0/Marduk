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
