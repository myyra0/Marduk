mod fixtures;

use std::cell::Cell;
use std::fs::{self, OpenOptions};
use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use fixtures::{ClassicZip, Record};
use marduk_archive::{AdmittedZip, ArchiveError, LimitKind, M1_ZIP_LIMITS, ZipLimits, admit_zip};

struct RefusalStage {
    scratch: tempfile::TempDir,
    staging: PathBuf,
}

impl RefusalStage {
    fn new() -> Self {
        let scratch = tempfile::tempdir().unwrap();
        let staging = scratch.path().join("stage");
        fs::create_dir(&staging).unwrap();
        fs::write(scratch.path().join("evil"), b"parent sentinel").unwrap();
        fs::create_dir(scratch.path().join("outside")).unwrap();
        fs::write(scratch.path().join("outside/keep"), b"outside sentinel").unwrap();
        Self { scratch, staging }
    }

    fn assert_refused(
        &self,
        case: &str,
        input: impl Read + Seek,
        limits: ZipLimits,
        expected: ArchiveError,
    ) {
        let mut opened_sink = false;
        let result = admit_zip(input, limits).and_then(|mut archive| {
            opened_sink = true;
            let mut out = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(self.staging.join("payload"))
                .unwrap();
            for index in 0..archive.entries().len() {
                archive.copy_entry(index, &mut out)?;
            }
            Ok(())
        });
        assert!(!opened_sink, "{case}: admission reached the staging sink");
        assert!(
            dir_names(&self.staging).is_empty(),
            "{case}: staging not empty"
        );
        let parent = self.scratch.path();
        assert_eq!(dir_names(parent), ["evil", "outside", "stage"], "{case}");
        assert_eq!(fs::read(parent.join("evil")).unwrap(), b"parent sentinel");
        assert_eq!(dir_names(&parent.join("outside")), ["keep"], "{case}");
        assert_eq!(
            fs::read(parent.join("outside/keep")).unwrap(),
            b"outside sentinel"
        );
        assert_eq!(result, Err(expected), "{case}");
    }
}

fn dir_names(path: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn admitted(zip: ClassicZip) -> AdmittedZip<Cursor<Vec<u8>>> {
    admit_zip(Cursor::new(zip.bytes), M1_ZIP_LIMITS)
        .unwrap_or_else(|error| panic!("expected admission, got {error:?}"))
}

fn file(name: &[u8]) -> Record<'_> {
    Record::file(name, b"fixture")
}

fn assert_content(archive: &mut AdmittedZip<impl Read + Seek>, expected: &[(&str, &[u8])]) {
    let manifest: Vec<_> = archive
        .entries()
        .iter()
        .map(|entry| (entry.path.as_str(), entry.uncompressed_len))
        .collect();
    assert_eq!(
        manifest,
        expected
            .iter()
            .map(|&(name, bytes)| (name, bytes.len() as u64))
            .collect::<Vec<_>>()
    );
    for (index, (_, expected)) in expected.iter().enumerate() {
        let mut out = Vec::new();
        assert_eq!(
            archive.copy_entry(index, &mut out),
            Ok(expected.len() as u64)
        );
        assert_eq!(&out, expected);
    }
}

#[test]
fn zip_exact_duplicate_is_not_a_last_wins_entry() {
    let zip = ClassicZip::new(&[
        Record::file(b"same.dds", b"first"),
        Record::file(b"same.dds", b"second"),
    ]);
    RefusalStage::new().assert_refused(
        "exact duplicate",
        Cursor::new(zip.bytes),
        M1_ZIP_LIMITS,
        ArchiveError::DuplicateRawName,
    );
}

#[test]
fn zip_alias_aborts_stage() {
    let stage = RefusalStage::new();
    let cases = [
        (
            "full path alias",
            vec![file(b"Textures/Winner.dds"), file(b"textures/winner.dds")],
            0,
            1,
        ),
        (
            "file then explicit directory",
            vec![file(b"Textures"), Record::dir(b"Textures/")],
            0,
            1,
        ),
        (
            "implied directory then file",
            vec![
                file(b"Textures/a.dds"),
                file(b"Textures/b.dds"),
                file(b"Textures"),
            ],
            0,
            2,
        ),
        (
            "different spelling of a canonical parent",
            vec![
                file(b"lead"),
                file(b"Data/Textures/a.dds"),
                file(b"Data/textures/b.dds"),
            ],
            1,
            2,
        ),
    ];
    for (case, records, first, second) in cases {
        stage.assert_refused(
            case,
            Cursor::new(ClassicZip::new(&records).bytes),
            M1_ZIP_LIMITS,
            ArchiveError::ConflictingNames { first, second },
        );
    }

    assert_content(
        &mut admitted(ClassicZip::new(&[Record::file(
            b"textures/winner.dds",
            b"b\n",
        )])),
        &[("textures/winner.dds", b"b\n")],
    );
}

#[test]
fn zip_untrusted_entry_never_creates_output() {
    let stage = RefusalStage::new();
    for (case, name) in [
        ("traversal", &b"../evil"[..]),
        ("absolute", b"/evil"),
        ("backslash", b"Textures\\evil"),
        ("NUL", b"evil\0.dds"),
        ("control", b"evil\x1f.dds"),
        ("device name", b"Textures/CON.dds"),
        ("invalid UTF-8", b"evil\xff.dds"),
    ] {
        let zip = ClassicZip::new(&[Record::dir(b"Textures/"), file(name)]);
        stage.assert_refused(
            case,
            Cursor::new(zip.bytes),
            M1_ZIP_LIMITS,
            ArchiveError::UnsafeName { index: 1 },
        );
    }

    for (case, bad) in [
        (
            "Unix symlink",
            Record {
                mode: 0o120777,
                ..Record::file(b"link", b"../outside/keep")
            },
        ),
        (
            "Unix FIFO",
            Record {
                mode: 0o010644,
                ..file(b"pipe")
            },
        ),
        (
            "unsupported method",
            Record {
                method: 12,
                ..file(b"bzip2")
            },
        ),
        (
            "encrypted",
            Record {
                flags: 1,
                ..file(b"encrypted")
            },
        ),
        (
            "directory marker without slash",
            Record::dir(b"not-a-directory"),
        ),
    ] {
        let zip = ClassicZip::new(&[Record::dir(b"Textures/"), bad]);
        stage.assert_refused(
            case,
            Cursor::new(zip.bytes),
            M1_ZIP_LIMITS,
            ArchiveError::UnsupportedEntry { index: 1 },
        );
    }
}

#[test]
fn zip_rejects_unbounded_or_corrupt_payload() {
    let stage = RefusalStage::new();
    // A DEFLATE block encoding 64 repetitions of "a" in only six bytes.
    let deflate = b"\x4b\x4c\xa4\x0c\x00\x00";
    for (case, limits, kind) in [
        (
            "archive bytes",
            ZipLimits {
                max_archive_bytes: 1,
                ..M1_ZIP_LIMITS
            },
            LimitKind::ArchiveBytes,
        ),
        (
            "expanded file bytes",
            ZipLimits {
                max_file_bytes: 63,
                ..M1_ZIP_LIMITS
            },
            LimitKind::FileBytes,
        ),
        (
            "total expanded bytes",
            ZipLimits {
                max_total_bytes: 68,
                ..M1_ZIP_LIMITS
            },
            LimitKind::TotalBytes,
        ),
        (
            "record count includes directory",
            ZipLimits {
                max_entries: 2,
                ..M1_ZIP_LIMITS
            },
            LimitKind::Entries,
        ),
    ] {
        let zip = ClassicZip::new(&[
            Record::dir(b"Data/"),
            Record::deflated(b"Data/compressed", &[b'a'; 64], deflate),
            Record::file(b"Data/plain", b"world"),
        ]);
        stage.assert_refused(
            case,
            Cursor::new(zip.bytes),
            limits,
            ArchiveError::LimitExceeded { kind },
        );
    }

    let mut oversized = ClassicZip::new(&[file(b"valid")]);
    oversized.set_eocd(
        12,
        &((M1_ZIP_LIMITS.max_directory_bytes + 1) as u32).to_le_bytes(),
    );
    stage.assert_refused(
        "oversized declared directory precedes extent validation",
        Cursor::new(oversized.bytes),
        M1_ZIP_LIMITS,
        ArchiveError::LimitExceeded {
            kind: LimitKind::DirectoryBytes,
        },
    );

    // Coherent ZIP extents enclose a DEFLATE block cut short inside its data.
    // This is a stream failure, not a local record running into the directory.
    let truncated = Record::deflated(b"Data/truncated", b"abcdefgh", b"\x01\x08\x00\xf7\xffabc");
    let mut archive = admitted(ClassicZip::new(&[Record::dir(b"Data/"), truncated]));
    assert_eq!(archive.entries().len(), 1);
    assert_eq!(archive.entries()[0].uncompressed_len, 8);
    assert_eq!(
        archive.copy_entry(0, &mut Vec::new()),
        Err(ArchiveError::CorruptEntry { index: 0 })
    );

    let wrong_length = Record {
        expanded_len: 8,
        ..Record::deflated(b"Data/wrong-length", &[b'a'; 64], deflate)
    };
    let mut archive = admitted(ClassicZip::new(&[wrong_length]));
    assert_eq!(archive.entries()[0].uncompressed_len, 8);
    let mut out = Vec::new();
    assert_eq!(
        archive.copy_entry(0, &mut out),
        Err(ArchiveError::CorruptEntry { index: 0 })
    );
    assert!(out.len() <= 8);

    let data = vec![b'x'; 1024 * 1024];
    let mut bad = Record::file(b"Data/bad-crc", &data);
    bad.crc ^= 1;
    let mut archive = admitted(ClassicZip::new(&[
        Record::dir(b"Data/"),
        Record::file(b"Data/valid", b"ok"),
        bad,
    ]));
    assert_eq!(archive.entries().len(), 2);
    let mut private = Vec::new();
    assert_eq!(archive.copy_entry(0, &mut private), Ok(2));
    assert_eq!(private, b"ok");

    let mut partial = Vec::new();
    // The caller may keep the bytes private, but only Ok can return them as prepared.
    let prepared = archive
        .copy_entry(1, &mut partial)
        .map(|_| (&private, &partial));
    assert_eq!(prepared, Err(ArchiveError::CorruptEntry { index: 1 }));
    assert!(
        !partial.is_empty(),
        "the integrity failure must follow some sink output"
    );
    assert!(data.starts_with(&partial));
}

#[test]
fn zip_golden_streams_in_central_directory_order() {
    let zip = ClassicZip::with_directory_order(
        &[
            Record::deflated(b"Middle.txt", b"hello", b"\xcb\x48\xcd\xc9\xc9\x07\x00"),
            Record::file(b"alpha", b"\x00\x7f\xff\n"),
            Record::file(b"Zed", b""),
        ],
        [2, 1, 0],
    );
    assert_content(
        &mut admitted(zip),
        &[
            ("Zed", b""),
            ("alpha", b"\x00\x7f\xff\n"),
            ("Middle.txt", b"hello"),
        ],
    );
}

#[test]
fn zip_explicit_parent_directory_is_not_content() {
    let zip = ClassicZip::new(&[
        Record::file(b"Data/Child", b"inside"),
        Record::dir(b"Data/"),
        Record::file(b"top", b"outside"),
    ]);
    assert_content(
        &mut admitted(zip),
        &[("Data/Child", b"inside"), ("top", b"outside")],
    );
}

#[test]
fn zip_preflight_does_not_read_the_directory_or_payload_to_enforce_caps() {
    // A full legal EOCD search window overlaps only the suffix of this directory.
    const EOCD_TAIL: usize = 22 + 65_536;
    let comment = [b'C'; 4096];
    let names: Vec<_> = (0..20).map(|idx| format!("file-{idx}")).collect();
    let records: Vec<_> = names
        .iter()
        .map(|name| Record {
            comment: &comment,
            ..file(name.as_bytes())
        })
        .collect();
    let stage = RefusalStage::new();
    for (limits, kind) in [
        (
            ZipLimits {
                max_directory_bytes: EOCD_TAIL as u64,
                max_entries: 19,
                ..M1_ZIP_LIMITS
            },
            LimitKind::DirectoryBytes,
        ),
        (
            ZipLimits {
                max_entries: 19,
                ..M1_ZIP_LIMITS
            },
            LimitKind::Entries,
        ),
    ] {
        let zip = ClassicZip::new(&records);
        let tail = zip.bytes.len() - EOCD_TAIL;
        assert!(tail > zip.directory_start);
        let reads = Cell::new(0);
        let blocked = Cell::new(false);
        let observed = TailReader {
            input: Cursor::new(zip.bytes),
            first_readable: tail as u64,
            budget: 2 * EOCD_TAIL,
            reads: &reads,
            blocked: &blocked,
        };
        stage.assert_refused(
            "preflight limit",
            observed,
            limits,
            ArchiveError::LimitExceeded { kind },
        );
        assert!(reads.get() > 0, "the limit depends on the EOCD bytes");
        assert!(
            !blocked.get(),
            "read attempted outside the EOCD tail or budget"
        );
    }
}

struct TailReader<'a> {
    input: Cursor<Vec<u8>>,
    first_readable: u64,
    budget: usize,
    reads: &'a Cell<usize>,
    blocked: &'a Cell<bool>,
}

impl Read for TailReader<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        if self.input.position() < self.first_readable || self.reads.get() >= self.budget {
            self.blocked.set(true);
            return Err(io::Error::other("outside EOCD read budget"));
        }
        let available = out.len().min(self.budget - self.reads.get());
        let len = self.input.read(&mut out[..available])?;
        self.reads.set(self.reads.get() + len);
        Ok(len)
    }
}

impl Seek for TailReader<'_> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.input.seek(pos)
    }
}

#[test]
fn zip_refuses_nonclassic_and_inconsistent_containers() {
    let stage = RefusalStage::new();
    let build = || ClassicZip::new(&[file(b"valid")]);
    let mut zip64 = build();
    zip64.set_eocd(8, &[0xff; 4]);
    let mut multidisk = build();
    multidisk.set_eocd(4, &1u16.to_le_bytes());
    let mut trailing = build().bytes;
    trailing.extend_from_slice(b"tail");
    let mut prepended = build();
    // Rebase both offsets, so the central-directory span is still exact.
    let cd = prepended.directory_start;
    prepended.bytes[cd + 42..cd + 46].copy_from_slice(&4u32.to_le_bytes());
    prepended.set_eocd(16, &(cd as u32 + 4).to_le_bytes());
    prepended.bytes.splice(0..0, *b"stub");
    let mut bad_span = build();
    bad_span.set_eocd(16, &(bad_span.directory_start as u32 + 1).to_le_bytes());
    let mut cut_extent = build();
    let cd = cut_extent.directory_start;
    // Both headers agree, but the declared stored extent overruns the directory start.
    for field in [18, 22, cd + 20, cd + 24] {
        cut_extent.bytes[field..field + 4].copy_from_slice(&8u32.to_le_bytes());
    }
    for (case, bytes, expected) in [
        (
            "ZIP64 sentinel",
            zip64.bytes,
            ArchiveError::UnsupportedContainer,
        ),
        (
            "multidisk",
            multidisk.bytes,
            ArchiveError::UnsupportedContainer,
        ),
        (
            "trailing data",
            trailing,
            ArchiveError::UnsupportedContainer,
        ),
        (
            "prepended data",
            prepended.bytes,
            ArchiveError::UnsupportedContainer,
        ),
        (
            "bad directory extent",
            bad_span.bytes,
            ArchiveError::InvalidContainer,
        ),
        (
            "truncated stored extent",
            cut_extent.bytes,
            ArchiveError::InvalidContainer,
        ),
        (
            "missing EOCD",
            b"not a zip".to_vec(),
            ArchiveError::InvalidContainer,
        ),
    ] {
        stage.assert_refused(case, Cursor::new(bytes), M1_ZIP_LIMITS, expected);
    }
}

#[test]
fn zip_validates_names_before_unsupported_metadata() {
    let stage = RefusalStage::new();
    for (name, expected) in [
        (&b"../evil"[..], ArchiveError::UnsafeName { index: 1 }),
        (b"lead", ArchiveError::DuplicateRawName),
        (
            b"Lead",
            ArchiveError::ConflictingNames {
                first: 0,
                second: 1,
            },
        ),
    ] {
        let zip = ClassicZip::new(&[
            file(b"lead"),
            Record {
                flags: 1,
                ..file(name)
            },
        ]);
        stage.assert_refused(
            "validation precedence",
            Cursor::new(zip.bytes),
            M1_ZIP_LIMITS,
            expected,
        );
    }
}

#[test]
fn zip_copy_reports_missing_manifest_index_and_sink_io() {
    #[derive(Default)]
    struct FailedSink(usize);

    impl Write for FailedSink {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            self.0 += 1;
            Err(io::Error::other("sink failure"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut archive = admitted(ClassicZip::new(&[
        Record::dir(b"Data/"),
        file(b"Data/item"),
    ]));
    assert_eq!(archive.entries().len(), 1);
    let mut sink = FailedSink::default();
    assert_eq!(
        archive.copy_entry(1, &mut sink),
        Err(ArchiveError::MissingEntry { index: 1 })
    );
    assert_eq!(sink.0, 0);
    assert_eq!(archive.copy_entry(0, &mut sink), Err(ArchiveError::Io));
    assert!(sink.0 > 0, "the error must originate from the sink");
}
