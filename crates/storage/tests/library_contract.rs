#[allow(dead_code)]
#[path = "../../archive/tests/fixtures/mod.rs"]
mod zip_fixture;

use std::path::Path;

use marduk_core::ids::{ProfileId, ProfileRevision};
use marduk_core::import::{BeginImport, ImportError, ImportId, ImportTarget, ProfileEditError};
use marduk_platform::WriterGate;
use marduk_storage::Catalog;

use zip_fixture::{ClassicZip, Record};

fn zip() -> Vec<u8> {
    ClassicZip::new(&[Record::file(b"synthetic.txt", b"private fixture bytes")]).bytes
}

fn append() -> BeginImport {
    BeginImport {
        profile: ProfileId::new(1),
        expected_revision: ProfileRevision::new(0),
        target: ImportTarget::Append {
            display_name: "Fixture".into(),
        },
    }
}

fn assert_initial(catalog: &mut Catalog) {
    let named = catalog.named_profile(ProfileId::new(1)).unwrap();
    assert_eq!(named.snapshot.id.get(), 1);
    assert_eq!(named.snapshot.revision.get(), 0);
    assert!(named.snapshot.members.is_empty());
    assert!(named.names.is_empty());
}

fn open(root: &Path) -> Catalog {
    Catalog::open(root).expect("open real SQLite catalog")
}

#[test]
fn fresh_catalog_seeds_only_the_initial_profile() {
    let tmp = tempfile::tempdir().unwrap();
    let mut catalog = open(tmp.path());
    assert_initial(&mut catalog);
    assert!(tmp.path().join("catalog.sqlite3").is_file());
    drop(catalog);
    assert_initial(&mut open(tmp.path()));
}

#[test]
fn production_stage_follows_the_qualified_volume() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let mut catalog = open(root);
    let staged = catalog.prepare_import(zip().as_slice(), append());

    if marduk_platform::library_volume_qualified(root) {
        let prepared = staged.expect("a qualified volume stages the archive");
        assert_eq!(prepared.profile, ProfileId::new(1));
        assert_initial(&mut catalog);
        drop(catalog);
        let mut reopened = open(root);
        assert_eq!(reopened.reconcile().unwrap(), vec![prepared.id]);
        assert_initial(&mut reopened);
    } else {
        assert_eq!(staged.unwrap_err(), ImportError::StageUnavailable);
        assert_initial(&mut catalog);
        drop(catalog);
        assert_initial(&mut open(root));
        assert_eq!(
            std::fs::read_dir(root.join("revisions")).unwrap().count(),
            0
        );
    }
}

#[test]
fn import_id_numeric_bounds_are_checked() {
    let maximum = u64::try_from(i64::MAX).unwrap();
    for value in [0, maximum + 1, u64::MAX] {
        assert_eq!(ImportId::try_from_u64(value), Err(ImportError::Storage));
    }
    for value in [1, maximum] {
        assert_eq!(ImportId::try_from_u64(value).unwrap().get(), value);
    }
}

#[test]
fn malformed_profile_ids_and_unknown_import_ids_are_refused() {
    let maximum = u64::try_from(i64::MAX).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut catalog = open(tmp.path());
    for value in [0, maximum + 1] {
        assert_eq!(
            catalog.snapshot(ProfileId::new(value)).unwrap_err(),
            ImportError::Storage,
        );
    }
    for value in [1, maximum] {
        let id = ImportId::try_from_u64(value).unwrap();
        assert_eq!(
            catalog.publish_import(id).unwrap_err(),
            ImportError::UnknownImport
        );
    }
    assert_initial(&mut catalog);
}

#[test]
fn second_catalog_observes_the_active_writer_gate() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let mut first = open(root);
    let mut second = open(root);
    let gate = WriterGate::open(root.join("library.lock")).unwrap();
    let guard = gate.try_lock().unwrap();
    assert!(matches!(Catalog::open(root), Err(ImportError::Busy)));
    let unknown = ImportId::try_from_u64(1).unwrap();
    assert_eq!(
        second
            .prepare_import(zip().as_slice(), append())
            .unwrap_err(),
        ImportError::Busy
    );
    assert_eq!(
        second.publish_import(unknown).unwrap_err(),
        ImportError::Busy
    );
    assert_eq!(second.discard_import(unknown), Err(ImportError::Busy));
    assert_eq!(second.reconcile().unwrap_err(), ImportError::Busy);
    assert_eq!(
        second
            .reorder(ProfileId::new(1), ProfileRevision::new(0), &[])
            .unwrap_err(),
        ProfileEditError::Busy
    );
    drop(guard);
    assert_initial(&mut first);
    assert_initial(&mut second);
    let staged = second.prepare_import(zip().as_slice(), append());
    if marduk_platform::library_volume_qualified(root) {
        assert!(staged.is_ok(), "a qualified volume must stage");
    } else {
        assert_eq!(staged.unwrap_err(), ImportError::StageUnavailable);
    }
}
