//! One catalog must remain openable by both storage front doors after shared
//! schema ownership was introduced.

use marduk_core::ids::ProfileId;
use marduk_core::recovery::Journal;
use marduk_core::root_identity::{NativeIdentity, RootIdentity};
use marduk_storage::Catalog;
use marduk_storage::operation_journal::OperationJournal;
use rusqlite::Connection;
use tempfile::TempDir;

fn root(id: u8) -> RootIdentity {
    RootIdentity::from_adapter_evidence(NativeIdentity::Windows {
        volume_serial: 71,
        file_id: [id; 16],
        creation_100ns: 201,
    })
}

fn revision(catalog: &mut Catalog) -> u64 {
    catalog.snapshot(ProfileId::new(1)).unwrap().revision.get()
}

#[test]
fn catalog_and_journal_share_one_v2_schema() {
    let dir: TempDir = tempfile::tempdir().unwrap();

    let mut catalog = Catalog::open(dir.path()).unwrap();
    assert_eq!(revision(&mut catalog), 0);
    drop(catalog);

    let mut journal = OperationJournal::open(dir.path()).unwrap();
    journal
        .select_verified_installation(std::path::Path::new("synthetic"), &root(1), &root(2))
        .unwrap();
    assert!(journal.root_state(&root(2)).unwrap().pending.is_none());
    drop(journal);

    let mut catalog = Catalog::open(dir.path()).unwrap();
    assert_eq!(revision(&mut catalog), 0);
    drop(catalog);

    let mut journal = OperationJournal::open(dir.path()).unwrap();
    assert!(journal.root_state(&root(2)).unwrap().pending.is_none());

    let db = Connection::open(dir.path().join("catalog.sqlite3")).unwrap();
    let version: i64 = db
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 2);
}

#[cfg(windows)]
#[test]
fn catalog_root_pin_and_native_stage_coexist() {
    let dir: TempDir = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(dir.path()).unwrap();
    // The catalog's pinned app root shares write, so the stage can bind the
    // same directory while the catalog stays open.
    let stage = marduk_platform::library_stage::NativeStage::open(dir.path())
        .expect("native stage must open beside the catalog root pin");
    drop(stage);
    drop(catalog);
}

#[cfg(windows)]
#[test]
fn catalog_and_journal_open_together() {
    let dir: TempDir = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(dir.path()).unwrap();
    let journal = OperationJournal::open(dir.path()).unwrap();
    drop(journal);
    drop(catalog);
}
