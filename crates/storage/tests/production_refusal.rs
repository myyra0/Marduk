//! Production refusal coverage for the unqualified stage adapter. The frozen
//! oracle lives in `library_contract.rs` and is not modified here.

use marduk_core::ids::ProfileId;
use marduk_core::import::{ImportError, ImportId};
use marduk_storage::Catalog;

#[test]
fn production_discard_and_reconcile_leave_unqualified_stage_rows_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let mut catalog = Catalog::open(root).expect("open production catalog");

    assert_eq!(catalog.reconcile().unwrap(), Vec::<ImportId>::new());

    // Production cannot prepare, so seed a row that only a stage adapter could
    // have produced and check that the unavailable adapter refuses it.
    {
        let connection = rusqlite::Connection::open(root.join("catalog.sqlite3")).unwrap();
        connection
            .execute(
                "INSERT INTO imports \
                 (id, profile_id, expected_revision, target, display_name, state, owner, archive_sha) \
                 VALUES (42, 1, 0, 'append', 'Orphan', 'prepared', x'01', ?1)",
                [vec![0_u8; 32]],
            )
            .unwrap();
    }

    let id = ImportId::try_from_u64(42).unwrap();
    assert_eq!(
        catalog.discard_import(id),
        Err(ImportError::StageUnavailable)
    );
    assert_eq!(catalog.reconcile().unwrap(), Vec::<ImportId>::new());

    let connection = rusqlite::Connection::open(root.join("catalog.sqlite3")).unwrap();
    let (state, result_revision): (String, Option<i64>) = connection
        .query_row(
            "SELECT state, result_revision FROM imports WHERE id = 42",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, "prepared");
    assert_eq!(result_revision, None);

    let installed: i64 = connection
        .query_row("SELECT count(*) FROM installed_revisions", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(installed, 0);

    let named = catalog.named_profile(ProfileId::new(1)).unwrap();
    assert_eq!(named.snapshot.revision.get(), 0);
    assert!(named.snapshot.members.is_empty());
}
