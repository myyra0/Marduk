//! Abandoned-import recovery witnesses.
//!
//! A refused or interrupted import must leave either a reclaimable owned
//! reservation or no row at all. These tests use the production `Catalog`, so
//! the reclamation half runs only on a volume the availability record qualifies
//! for private staging; everywhere else the same rows must be quarantined and
//! their bytes preserved.

#[allow(dead_code)]
#[path = "../../archive/tests/fixtures/mod.rs"]
mod zip_fixture;

use std::fs;
use std::path::{Path, PathBuf};

use marduk_core::ids::{ProfileId, ProfileRevision};
use marduk_core::import::{BeginImport, ImportError, ImportId, ImportTarget};
use marduk_storage::library::{QuarantineReason, QuarantinedImport};
use marduk_storage::Catalog;
use rusqlite::{params, Connection, OptionalExtension};
use zip_fixture::{ClassicZip, Record};

const FILE: &str = "textures/synthetic.dds";

fn zip(bytes: &[u8]) -> Vec<u8> {
    ClassicZip::new(&[Record::file(FILE.as_bytes(), bytes)]).bytes
}

fn plan() -> BeginImport {
    BeginImport {
        profile: ProfileId::new(1),
        expected_revision: ProfileRevision::new(0),
        target: ImportTarget::Append {
            display_name: "Recovery".into(),
        },
    }
}

fn revision_dir(root: &Path, id: ImportId) -> PathBuf {
    root.join("revisions").join(id.get().to_string())
}

/// Create a revision directory the adapter will accept as a binding target:
/// `open_revision` proves the private 0700 mode before it trusts the name.
fn private_dir(path: &Path) {
    fs::create_dir_all(path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
}

fn database(root: &Path) -> Connection {
    Connection::open(root.join("catalog.sqlite3")).unwrap()
}

fn sql_id(id: ImportId) -> i64 {
    i64::try_from(id.get()).unwrap()
}

fn import_id(root: &Path, state: &str) -> Option<ImportId> {
    let value: Option<i64> = database(root)
        .query_row(
            "SELECT id FROM imports WHERE state = ?1 ORDER BY id DESC LIMIT 1",
            [state],
            |row| row.get(0),
        )
        .optional()
        .unwrap();
    value.map(|value| ImportId::try_from_u64(u64::try_from(value).unwrap()).unwrap())
}

fn state_of(root: &Path, id: ImportId) -> String {
    database(root)
        .query_row(
            "SELECT state FROM imports WHERE id = ?1",
            [sql_id(id)],
            |row| row.get(0),
        )
        .unwrap()
}

fn writing_rows(root: &Path) -> i64 {
    database(root)
        .query_row(
            "SELECT count(*) FROM imports WHERE state = 'writing'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

/// Seed a reservation that no adapter can prove, the shape a catalog without a
/// complete receipt would hold.
fn seed_unprovable_writing(root: &Path, id: ImportId, bytes: &[u8]) {
    let directory = revision_dir(root, id);
    private_dir(&directory);
    fs::create_dir_all(directory.join("payload")).unwrap();
    fs::write(directory.join("archive.zip"), bytes).unwrap();
    fs::write(directory.join("payload").join("partial.bin"), bytes).unwrap();
    database(root)
        .execute(
            "INSERT INTO imports \
             (id, profile_id, expected_revision, target, display_name, state) \
             VALUES (?1, 1, 0, 'append', 'Orphan', 'writing')",
            [sql_id(id)],
        )
        .unwrap();
}

/// Build an empty reservation through the production adapter, then turn it into
/// the shape a crash leaves before any archive byte: the row lost its ownership
/// receipt and the directory lost its contents, while the directory keeps the
/// identity the adapter created and verified. The unowned reclaim only removes
/// a name it can prove is a private revision, so a hand-made directory is not
/// that shape.
fn seed_staged_empty_reservation(catalog: &mut Catalog, root: &Path, bytes: &[u8]) -> ImportId {
    let id = catalog
        .prepare_import(zip(bytes).as_slice(), plan())
        .unwrap()
        .id;

    let db = database(root);
    db.execute(
        "DELETE FROM staged_files WHERE import_id = ?1",
        [sql_id(id)],
    )
    .unwrap();
    db.execute(
        "UPDATE imports SET state = 'writing', owner = NULL, archive_sha = NULL \
         WHERE id = ?1",
        [sql_id(id)],
    )
    .unwrap();

    let directory = revision_dir(root, id);
    fs::remove_dir_all(directory.join("payload")).unwrap();
    fs::remove_file(directory.join("archive.zip")).unwrap();
    id
}

/// Seed a `writing` row with no ownership receipt. The caller decides whether
/// `revisions/<id>` exists and what it holds, so each witness can pin the shape
/// the adapter is asked to reclaim. Only usable where no adapter can qualify the
/// directory, because an unverified name is never reclaimable.
fn seed_unowned_writing(root: &Path, id: ImportId) {
    database(root)
        .execute(
            "INSERT INTO imports \
             (id, profile_id, expected_revision, target, display_name, state) \
             VALUES (?1, 1, 0, 'append', 'Unowned', 'writing')",
            [sql_id(id)],
        )
        .unwrap();
}

fn mentions(report: &marduk_storage::library::ReconcileReport, id: ImportId) -> usize {
    usize::from(report.reclaimed.contains(&id))
        + report
            .quarantined
            .iter()
            .filter(|entry| entry.id == id)
            .count()
}

#[test]
fn refused_import_leaves_no_private_snapshot_after_reconcile() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let mut catalog = Catalog::open(root).unwrap();
    let staged = catalog.stage_available();

    // The exact gate case: a container whose admission fails after the archive
    // bytes were already streamed into the private revision directory.
    let mut archive = zip(b"bytes that are never admitted");
    archive.truncate(archive.len() - 22);
    let error = catalog
        .prepare_import(archive.as_slice(), plan())
        .unwrap_err();

    if staged {
        assert!(
            matches!(error, ImportError::Archive(_)),
            "a qualified stage must refuse the container after staging: {error:?}"
        );
        let id = import_id(root, "writing").expect("the refusal leaves a reservation behind");
        assert!(revision_dir(root, id).exists());

        let report = catalog.reconcile().unwrap();
        assert_eq!(report.reclaimed, vec![id]);
        assert!(report.quarantined.is_empty(), "{report:?}");
        assert!(!revision_dir(root, id).exists());
        assert_eq!(state_of(root, id), "discarded");
    } else {
        assert_eq!(error, ImportError::StageUnavailable);
        let report = catalog.reconcile().unwrap();
        assert!(report.reclaimed.is_empty());
        assert!(report.quarantined.is_empty());
        assert_eq!(
            fs::read_dir(root.join("revisions")).unwrap().count(),
            0,
            "an unqualified volume writes nothing"
        );
    }

    assert_eq!(writing_rows(root), 0);
    assert!(catalog
        .snapshot(ProfileId::new(1))
        .unwrap()
        .members
        .is_empty());
}

#[test]
fn owned_abandoned_reservation_is_reclaimed_and_unowned_bytes_are_quarantined() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let mut catalog = Catalog::open(root).unwrap();

    if !catalog.stage_available() {
        // No qualified stage: a writing row with bytes is never provable, and
        // the unavailable adapter must preserve it instead of guessing.
        let id = ImportId::try_from_u64(77).unwrap();
        seed_unprovable_writing(root, id, b"partial bytes");
        let report = catalog.reconcile().unwrap();
        assert!(report.reclaimed.is_empty());
        assert_eq!(
            report.quarantined,
            vec![QuarantinedImport {
                id,
                reason: QuarantineReason::StageUnavailable,
            }]
        );
        assert!(revision_dir(root, id).exists());
        assert_eq!(state_of(root, id), "writing");
        return;
    }

    // An owned reservation whose receipt never completed: the row records the
    // native directory identity, but the archive digest and file rows never
    // reached the catalog.
    let owned = catalog
        .prepare_import(zip(b"owned partial bytes").as_slice(), plan())
        .unwrap()
        .id;
    database(root)
        .execute(
            "DELETE FROM staged_files WHERE import_id = ?1",
            [sql_id(owned)],
        )
        .unwrap();
    database(root)
        .execute(
            "UPDATE imports SET state = 'writing', archive_sha = NULL WHERE id = ?1",
            [sql_id(owned)],
        )
        .unwrap();
    assert!(revision_dir(root, owned).exists());

    // The same crash shape without an owner receipt: bytes exist, nothing
    // proves they are ours.
    let unowned = catalog
        .prepare_import(zip(b"unowned partial bytes").as_slice(), plan())
        .unwrap()
        .id;
    database(root)
        .execute(
            "DELETE FROM staged_files WHERE import_id = ?1",
            [sql_id(unowned)],
        )
        .unwrap();
    database(root)
        .execute(
            "UPDATE imports SET state = 'writing', archive_sha = NULL, owner = NULL \
             WHERE id = ?1",
            [sql_id(unowned)],
        )
        .unwrap();

    let report = catalog.reconcile().unwrap();
    assert_eq!(report.reclaimed, vec![owned]);
    assert_eq!(
        report.quarantined,
        vec![QuarantinedImport {
            id: unowned,
            reason: QuarantineReason::OwnershipMissing,
        }]
    );
    assert!(!revision_dir(root, owned).exists());
    assert_eq!(state_of(root, owned), "discarded");

    let preserved = revision_dir(root, unowned);
    assert!(preserved.exists(), "unproven bytes are never reclaimed");
    assert!(preserved.join("archive.zip").is_file());
    assert_eq!(state_of(root, unowned), "writing");

    // A second pass is deterministic: the tombstone is gone, the ambiguous row
    // is reported again and its bytes still remain.
    let second = catalog.reconcile().unwrap();
    assert!(second.reclaimed.is_empty());
    assert_eq!(
        second.quarantined,
        vec![QuarantinedImport {
            id: unowned,
            reason: QuarantineReason::OwnershipMissing,
        }]
    );
    assert!(preserved.exists());
}

#[test]
fn committed_and_prepared_imports_are_never_reclaimed() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let mut catalog = Catalog::open(root).unwrap();

    if catalog.stage_available() {
        let prepared = catalog
            .prepare_import(zip(b"prepared bytes").as_slice(), plan())
            .unwrap()
            .id;
        let committed = catalog
            .prepare_import(zip(b"committed bytes").as_slice(), plan())
            .unwrap()
            .id;
        catalog.publish_import(committed).unwrap();

        let report = catalog.reconcile().unwrap();
        assert_eq!(report.prepared, vec![prepared]);
        assert!(report.reclaimed.is_empty());
        assert!(report.quarantined.is_empty(), "{report:?}");
        assert!(revision_dir(root, prepared).exists());
        assert!(revision_dir(root, committed).exists());
        assert_eq!(
            fs::read(revision_dir(root, committed).join("payload").join(FILE)).unwrap(),
            b"committed bytes"
        );
        return;
    }

    // Without a qualified adapter, seeded prepared and committed rows are left
    // exactly as they are; a committed revision is never a reconciliation
    // candidate at all.
    let prepared = ImportId::try_from_u64(81).unwrap();
    let committed = ImportId::try_from_u64(82).unwrap();
    let sha = [0_u8; 32];
    let db = database(root);
    db.execute(
        "INSERT INTO imports \
         (id, profile_id, expected_revision, target, display_name, state, owner, archive_sha) \
         VALUES (?1, 1, 0, 'append', 'Prepared', 'prepared', x'01', ?2)",
        params![sql_id(prepared), sha.as_slice()],
    )
    .unwrap();
    db.execute(
        "INSERT INTO imports \
         (id, profile_id, expected_revision, target, display_name, state, owner, archive_sha, \
          result_revision) \
         VALUES (?1, 1, 0, 'append', 'Committed', 'committed', x'01', ?2, 1)",
        params![sql_id(committed), sha.as_slice()],
    )
    .unwrap();
    db.execute(
        "INSERT INTO installed_revisions (id) VALUES (?1)",
        [sql_id(committed)],
    )
    .unwrap();
    for id in [prepared, committed] {
        fs::create_dir_all(revision_dir(root, id).join("payload")).unwrap();
        fs::write(
            revision_dir(root, id).join("archive.zip"),
            b"retained bytes",
        )
        .unwrap();
    }

    let report = catalog.reconcile().unwrap();
    assert!(report.prepared.is_empty());
    assert!(report.reclaimed.is_empty());
    assert_eq!(
        report.quarantined,
        vec![QuarantinedImport {
            id: prepared,
            reason: QuarantineReason::StageUnavailable,
        }]
    );
    assert!(revision_dir(root, prepared).exists());
    assert!(revision_dir(root, committed).exists());
}

#[test]
fn pre_fix_writing_row_is_handled_deterministically() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let mut catalog = Catalog::open(root).unwrap();

    let id = if catalog.stage_available() {
        // A complete-looking reservation from before the receipt was persisted
        // before staging: owner recorded, digest absent, no file rows.
        let id = catalog
            .prepare_import(zip(b"pre-fix bytes").as_slice(), plan())
            .unwrap()
            .id;
        database(root)
            .execute(
                "DELETE FROM staged_files WHERE import_id = ?1",
                [sql_id(id)],
            )
            .unwrap();
        database(root)
            .execute(
                "UPDATE imports SET state = 'writing', archive_sha = NULL WHERE id = ?1",
                [sql_id(id)],
            )
            .unwrap();
        id
    } else {
        let id = ImportId::try_from_u64(91).unwrap();
        seed_unprovable_writing(root, id, b"pre-fix bytes");
        id
    };

    let report = catalog.reconcile().unwrap();
    assert_eq!(
        mentions(&report, id),
        1,
        "the pre-fix row is reported exactly once, never silently dropped: {report:?}"
    );
    if catalog.stage_available() {
        assert_eq!(report.reclaimed, vec![id]);
        assert!(!revision_dir(root, id).exists());
        assert_eq!(state_of(root, id), "discarded");
    } else {
        assert_eq!(
            report.quarantined,
            vec![QuarantinedImport {
                id,
                reason: QuarantineReason::StageUnavailable,
            }],
            "an unprovable pre-fix row is quarantined, not reclaimed"
        );
        assert!(revision_dir(root, id).exists());
        assert_eq!(state_of(root, id), "writing");
    }
}

#[test]
fn unowned_empty_reservation_is_removed() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let mut catalog = Catalog::open(root).unwrap();
    let available = catalog.stage_available();

    let id = if available {
        seed_staged_empty_reservation(&mut catalog, root, b"reserved bytes")
    } else {
        let id = ImportId::try_from_u64(101).unwrap();
        private_dir(&revision_dir(root, id));
        seed_unowned_writing(root, id);
        id
    };

    let report = catalog.reconcile().unwrap();
    if available {
        assert_eq!(report.reclaimed, vec![id], "{report:?}");
        assert!(report.quarantined.is_empty(), "{report:?}");
        assert!(
            !revision_dir(root, id).exists(),
            "an empty name may be removed"
        );
        assert_eq!(state_of(root, id), "discarded");
        assert_eq!(writing_rows(root), 0);
    } else {
        assert!(report.reclaimed.is_empty());
        assert_eq!(
            report.quarantined,
            vec![QuarantinedImport {
                id,
                reason: QuarantineReason::StageUnavailable,
            }]
        );
        assert!(revision_dir(root, id).exists());
        assert_eq!(state_of(root, id), "writing");
    }
}

#[test]
fn unowned_reservation_that_gains_bytes_before_reconcile_is_quarantined_intact() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let mut catalog = Catalog::open(root).unwrap();
    let available = catalog.stage_available();

    let id = ImportId::try_from_u64(102).unwrap();
    private_dir(&revision_dir(root, id));
    seed_unowned_writing(root, id);

    // The name is empty when the row is written, then gains bytes immediately
    // before reconcile. An unowned reclaim may remove at most the empty
    // directory name; it must never enumerate or delete what it finds, so no
    // sequence here can recurse.
    let nested = revision_dir(root, id).join("payload").join("late");
    fs::create_dir_all(&nested).unwrap();
    fs::write(nested.join("incoming.bin"), b"arrived after the row").unwrap();

    let reason = if available {
        QuarantineReason::OwnershipMissing
    } else {
        QuarantineReason::StageUnavailable
    };
    let report = catalog.reconcile().unwrap();
    assert!(report.reclaimed.is_empty(), "{report:?}");
    assert_eq!(
        report.quarantined,
        vec![QuarantinedImport { id, reason }],
        "{report:?}"
    );
    assert_eq!(
        fs::read(nested.join("incoming.bin")).unwrap(),
        b"arrived after the row"
    );
    assert_eq!(state_of(root, id), "writing");

    // The quarantine is stable and the bytes are still all there afterwards.
    let second = catalog.reconcile().unwrap();
    assert!(second.reclaimed.is_empty(), "{second:?}");
    assert_eq!(second.quarantined, vec![QuarantinedImport { id, reason }]);
    assert_eq!(
        fs::read(nested.join("incoming.bin")).unwrap(),
        b"arrived after the row"
    );
}

#[test]
fn owned_reservation_reclaims_its_whole_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let mut catalog = Catalog::open(root).unwrap();

    if !catalog.stage_available() {
        // No receipt can exist without a qualified adapter, so there is no
        // owned reservation to reclaim; the refusal path is asserted instead.
        assert_eq!(
            catalog
                .prepare_import(zip(b"owned tree bytes").as_slice(), plan())
                .unwrap_err(),
            ImportError::StageUnavailable
        );
        return;
    }

    // A reservation whose receipt never completed: the recorded native
    // directory identity is the only proof, and it is enough to remove the
    // entire tree below that name.
    let id = catalog
        .prepare_import(zip(b"owned tree bytes").as_slice(), plan())
        .unwrap()
        .id;
    database(root)
        .execute(
            "DELETE FROM staged_files WHERE import_id = ?1",
            [sql_id(id)],
        )
        .unwrap();
    database(root)
        .execute(
            "UPDATE imports SET state = 'writing', archive_sha = NULL WHERE id = ?1",
            [sql_id(id)],
        )
        .unwrap();

    // Entries the receipt never named, at a depth the archive never used.
    let nested = revision_dir(root, id).join("scratch").join("deep");
    fs::create_dir_all(&nested).unwrap();
    fs::write(nested.join("orphan.bin"), b"proved ours").unwrap();

    let report = catalog.reconcile().unwrap();
    assert_eq!(report.reclaimed, vec![id], "{report:?}");
    assert!(report.quarantined.is_empty(), "{report:?}");
    assert!(
        !revision_dir(root, id).exists(),
        "a proved owner reclaims the whole tree"
    );
    assert_eq!(state_of(root, id), "discarded");
}
