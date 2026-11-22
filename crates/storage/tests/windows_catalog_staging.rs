//! NTFS catalog staging through the production `Catalog::open`.
//!
//! The catalog's app-root pin and the native stage must coexist: the pin shares
//! read and write but denies delete sharing, so the stage can create inside the
//! same directory while the catalog stays open.

#![cfg(windows)]

#[allow(dead_code)]
#[path = "../../archive/tests/fixtures/mod.rs"]
mod zip_fixture;

use std::fs;
use std::path::Path;

use marduk_core::ids::{ProfileId, ProfileRevision};
use marduk_core::import::{BeginImport, ImportId, ImportTarget};
use marduk_storage::Catalog;
use zip_fixture::{ClassicZip, Record};

const FILE: &str = "textures/synthetic.dds";
const BYTES: &[u8] = b"ntfs private stage bytes";

fn zip() -> Vec<u8> {
    ClassicZip::new(&[Record::file(FILE.as_bytes(), BYTES)]).bytes
}

fn append() -> BeginImport {
    BeginImport {
        profile: ProfileId::new(1),
        expected_revision: ProfileRevision::new(0),
        target: ImportTarget::Append {
            display_name: "NTFS".into(),
        },
    }
}

fn revision_dir(root: &Path, id: ImportId) -> std::path::PathBuf {
    root.join("revisions").join(id.get().to_string())
}

#[test]
fn windows_catalog_stages_a_payload_beside_its_root_pin() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let mut catalog = Catalog::open(root).unwrap();
    assert!(
        catalog.stage_available(),
        "the record must qualify NTFS import for this witness"
    );

    let prepared = catalog.prepare_import(zip().as_slice(), append()).unwrap();
    assert_eq!(prepared.profile, ProfileId::new(1));
    assert_eq!(prepared.files.len(), 1);
    assert_eq!(prepared.files[0].relative.as_str(), FILE);

    let revision = revision_dir(root, prepared.id);
    assert_eq!(
        fs::read(revision.join("payload").join(FILE)).unwrap(),
        BYTES
    );
    assert_eq!(catalog.reconcile().unwrap(), vec![prepared.id]);
}
