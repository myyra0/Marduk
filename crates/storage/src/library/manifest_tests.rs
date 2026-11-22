//! Regression for distinguishing an unknown revision from a published revision
//! with no manifest rows. Kept out of the frozen `contract_tests.rs` oracle.

#[allow(dead_code)]
#[path = "../../../archive/tests/fixtures/mod.rs"]
mod zip_fixture;

use marduk_core::ids::{InstalledRevisionId, ProfileId, ProfileRevision};
use marduk_core::import::{BeginImport, ImportTarget};

use super::Catalog;
use zip_fixture::{ClassicZip, Record};

const FILE: &str = "textures/synthetic.dds";
const A: &[u8] = b"published bytes";

#[test]
fn resolved_inputs_distinguishes_unknown_from_empty_revisions() {
    let tmp = tempfile::tempdir().unwrap();
    let mut catalog = Catalog::open_for_test(tmp.path()).unwrap();
    let archive = ClassicZip::new(&[Record::file(FILE.as_bytes(), A)]).bytes;
    let prepared = catalog
        .prepare_import(
            archive.as_slice(),
            BeginImport {
                profile: ProfileId::new(1),
                expected_revision: ProfileRevision::new(0),
                target: ImportTarget::Append {
                    display_name: "A".to_owned(),
                },
            },
        )
        .unwrap();
    let a = prepared.id;
    catalog.publish_import(a).unwrap();
    let installed = InstalledRevisionId::new(a.get());
    let inputs = catalog.resolved_inputs(ProfileId::new(1)).unwrap();
    assert_eq!(
        inputs.manifests.get(&installed).map(Vec::len),
        Some(1),
        "a published revision with rows has a manifest"
    );

    catalog
        .connection
        .execute(
            "DELETE FROM revision_files WHERE revision_id = ?1",
            [i64::try_from(a.get()).unwrap()],
        )
        .unwrap();
    let inputs = catalog.resolved_inputs(ProfileId::new(1)).unwrap();
    assert_eq!(
        inputs.manifests.get(&installed).map(Vec::len),
        Some(0),
        "a published revision with no rows is an empty manifest"
    );

    catalog
        .connection
        .execute("PRAGMA foreign_keys = OFF", [])
        .unwrap();
    catalog
        .connection
        .execute(
            "DELETE FROM installed_revisions WHERE id = ?1",
            [i64::try_from(a.get()).unwrap()],
        )
        .unwrap();
    catalog
        .connection
        .execute("PRAGMA foreign_keys = ON", [])
        .unwrap();
    let inputs = catalog.resolved_inputs(ProfileId::new(1)).unwrap();
    assert!(
        inputs.manifests.get(&installed).is_none(),
        "an unknown revision has no manifest at all"
    );
}
