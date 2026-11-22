//! Qualified-volume private staging through the production `Catalog::open`.
//!
//! The witness refuses to pass on an unqualified volume: it asserts the volume
//! class instead of skipping, so an ext4 or NTFS run fails loudly.

#![cfg(target_os = "linux")]

#[allow(dead_code)]
#[path = "../../archive/tests/fixtures/mod.rs"]
mod zip_fixture;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use marduk_core::ids::{ProfileId, ProfileRevision};
use marduk_core::import::{BeginImport, ImportId, ImportTarget};
use marduk_storage::Catalog;
use zip_fixture::{ClassicZip, Record};

const FILE: &str = "textures/synthetic.dds";
const BYTES: &[u8] = b"qualified private stage bytes";

fn zip() -> Vec<u8> {
    ClassicZip::new(&[Record::file(FILE.as_bytes(), BYTES)]).bytes
}

fn append() -> BeginImport {
    BeginImport {
        profile: ProfileId::new(1),
        expected_revision: ProfileRevision::new(0),
        target: ImportTarget::Append {
            display_name: "Qualified".into(),
        },
    }
}

fn revision_dir(root: &Path, id: ImportId) -> PathBuf {
    root.join("revisions").join(id.get().to_string())
}

#[test]
fn qualified_volume_stages_the_payload_through_the_production_catalog() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    assert!(
        marduk_platform::library_volume_qualified(root),
        "this witness needs a qualified volume; point TMPDIR at tmpfs or Btrfs"
    );

    let mut catalog = Catalog::open(root).unwrap();
    assert!(catalog.stage_available());
    let prepared = catalog.prepare_import(zip().as_slice(), append()).unwrap();

    assert_eq!(prepared.profile, ProfileId::new(1));
    assert_eq!(prepared.files.len(), 1);
    assert_eq!(prepared.files[0].relative.as_str(), FILE);

    let revision = revision_dir(root, prepared.id);
    assert_eq!(
        fs::metadata(&revision).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert!(revision.join("archive.zip").is_file());
    assert_eq!(
        fs::read(revision.join("payload").join(FILE)).unwrap(),
        BYTES
    );

    // A prepared stage never becomes a visible revision.
    let named = catalog.named_profile(ProfileId::new(1)).unwrap();
    assert!(named.snapshot.members.is_empty());
    assert!(named.names.is_empty());

    // The receipt is re-observed against the staged bytes by the same adapter.
    assert_eq!(catalog.reconcile().unwrap(), vec![prepared.id]);
}
