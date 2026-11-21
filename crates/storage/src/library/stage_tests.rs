//! Focused tests for the private stage seam added by the Task 3 review fixes.
//! The frozen oracle lives in `contract_tests.rs` and is not modified here.

#[allow(dead_code)]
#[path = "../../../archive/tests/fixtures/mod.rs"]
mod zip_fixture;

use std::path::{Path, PathBuf};

use marduk_core::ids::{MemberId, ProfileId, ProfileRevision};
use marduk_core::import::{
    ArchiveError, BeginImport, ImportError, ImportId, ImportTarget, PreparedImport,
};

use super::Catalog;
use zip_fixture::{ClassicZip, Record};

const FILE: &str = "textures/synthetic.dds";
const A: &[u8] = b"committed bytes";

fn profile() -> ProfileId {
    ProfileId::new(1)
}

fn append(name: &str) -> ImportTarget {
    ImportTarget::Append {
        display_name: name.to_owned(),
    }
}

fn plan(revision: u64, target: ImportTarget) -> BeginImport {
    BeginImport {
        profile: profile(),
        expected_revision: ProfileRevision::new(revision),
        target,
    }
}

fn zip(data: &[u8]) -> Vec<u8> {
    ClassicZip::new(&[Record::file(FILE.as_bytes(), data)]).bytes
}

fn prepare(
    catalog: &mut Catalog,
    revision: u64,
    target: ImportTarget,
    bytes: &[u8],
) -> PreparedImport {
    let archive = zip(bytes);
    catalog
        .prepare_import(archive.as_slice(), plan(revision, target))
        .expect("valid fixture ZIP must prepare")
}

fn revision_dir(root: &Path, id: ImportId) -> PathBuf {
    root.join("revisions").join(id.get().to_string())
}

fn payload(root: &Path, id: ImportId) -> PathBuf {
    revision_dir(root, id).join("payload").join(FILE)
}

fn state(catalog: &Catalog, id: ImportId) -> String {
    catalog
        .connection
        .query_row(
            "SELECT state FROM imports WHERE id = ?1",
            [i64::try_from(id.get()).unwrap()],
            |row| row.get(0),
        )
        .unwrap()
}

fn published(catalog: &Catalog) -> Vec<i64> {
    catalog
        .connection
        .prepare("SELECT id FROM installed_revisions ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn import_count(catalog: &Catalog) -> i64 {
    catalog
        .connection
        .query_row("SELECT count(*) FROM imports", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn reconcile_verifies_prepared_stage_integrity() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let mut catalog = Catalog::open_for_test(root).unwrap();

    let a = prepare(&mut catalog, 0, append("A"), A).id;
    let b = prepare(&mut catalog, 0, append("B"), b"tampered later").id;
    std::fs::write(payload(root, b), vec![b'X'; b"tampered later".len()]).unwrap();

    assert_eq!(catalog.reconcile().unwrap(), [a]);
    assert_eq!(state(&catalog, a), "prepared");
    assert_eq!(state(&catalog, b), "prepared");
    assert!(published(&catalog).is_empty());
}

#[test]
fn discard_reports_leftover_and_retries_after_cleanup() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let mut catalog = Catalog::open_for_test(root).unwrap();
    let id = prepare(&mut catalog, 0, append("A"), A).id;
    let directory = revision_dir(root, id);
    let unexpected = directory.join("unexpected");
    std::fs::write(&unexpected, b"not ours").unwrap();

    assert_eq!(catalog.discard_import(id), Err(ImportError::StageChanged));
    assert_eq!(state(&catalog, id), "discarded");
    assert!(!directory.join("archive.zip").exists());
    assert!(!payload(root, id).exists());
    assert!(unexpected.exists());

    // Leftovers are reported again, never silently accepted as success.
    assert_eq!(catalog.discard_import(id), Err(ImportError::StageChanged));

    std::fs::remove_file(&unexpected).unwrap();
    catalog.discard_import(id).unwrap();
    assert!(!directory.exists());
    assert_eq!(catalog.discard_import(id), Ok(()));
}

#[test]
fn discarded_import_cannot_publish() {
    let tmp = tempfile::tempdir().unwrap();
    let mut catalog = Catalog::open_for_test(tmp.path()).unwrap();
    let id = prepare(&mut catalog, 0, append("A"), A).id;

    catalog.discard_import(id).unwrap();
    assert_eq!(state(&catalog, id), "discarded");
    assert_eq!(
        catalog.publish_import(id).unwrap_err(),
        ImportError::ImportNotPrepared
    );
}

#[test]
fn corrupted_staged_path_is_refused_before_publish() {
    let tmp = tempfile::tempdir().unwrap();
    let mut catalog = Catalog::open_for_test(tmp.path()).unwrap();
    let id = prepare(&mut catalog, 0, append("A"), A).id;
    catalog
        .connection
        .execute(
            "UPDATE staged_files SET path = '../escape' WHERE import_id = ?1",
            [i64::try_from(id.get()).unwrap()],
        )
        .unwrap();

    assert_eq!(
        catalog.publish_import(id).unwrap_err(),
        ImportError::Storage
    );
    assert!(published(&catalog).is_empty());
    assert_named(&mut catalog, 0, &[]);
}

fn assert_named(catalog: &mut Catalog, revision: u64, members: &[(&str, bool)]) {
    let named = catalog.named_profile(profile()).unwrap();
    assert_eq!(named.snapshot.revision.get(), revision);
    assert_eq!(named.snapshot.members.len(), members.len());
    for (member, &(name, enabled)) in named.snapshot.members.iter().zip(members) {
        assert_eq!(named.names.get(&member.id).map(String::as_str), Some(name));
        assert_eq!(member.enabled, enabled);
    }
}

#[test]
fn prepare_refuses_stale_plan_and_unknown_member_without_reserving() {
    let tmp = tempfile::tempdir().unwrap();
    let mut catalog = Catalog::open_for_test(tmp.path()).unwrap();
    let archive = zip(A);

    assert_eq!(
        catalog
            .prepare_import(archive.as_slice(), plan(1, append("Stale")))
            .unwrap_err(),
        ImportError::StalePlan
    );
    assert_eq!(import_count(&catalog), 0);

    assert_eq!(
        catalog
            .prepare_import(
                archive.as_slice(),
                plan(
                    0,
                    ImportTarget::Replace {
                        member: MemberId::new(99)
                    }
                )
            )
            .unwrap_err(),
        ImportError::MemberNotFound
    );
    assert_eq!(import_count(&catalog), 0);
}

#[test]
fn reconcile_reclaims_abandoned_owned_writing_and_keeps_published_payload() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let mut catalog = Catalog::open_for_test(root).unwrap();

    let a = prepare(&mut catalog, 0, append("A"), A).id;
    assert_eq!(catalog.publish_import(a).unwrap().members.len(), 1);

    let invalid = zip(b"abandoned");
    let truncated = &invalid[..invalid.len() - 22];
    assert_eq!(
        catalog
            .prepare_import(truncated, plan(1, append("Abandoned")))
            .unwrap_err(),
        ImportError::Archive(ArchiveError::InvalidContainer)
    );

    let abandoned = catalog
        .connection
        .query_row(
            "SELECT id FROM imports WHERE state = 'writing'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    let abandoned = ImportId::try_from_u64(u64::try_from(abandoned).unwrap()).unwrap();
    assert_eq!(state(&catalog, abandoned), "writing");

    assert!(catalog.reconcile().unwrap().is_empty());
    assert_eq!(state(&catalog, abandoned), "discarded");
    assert!(!revision_dir(root, abandoned).exists());

    assert_eq!(published(&catalog), [i64::try_from(a.get()).unwrap()]);
    assert_eq!(std::fs::read(payload(root, a)).unwrap(), A);
    assert_eq!(
        catalog
            .named_profile(profile())
            .unwrap()
            .names
            .values()
            .next()
            .map(String::as_str),
        Some("A")
    );
}
