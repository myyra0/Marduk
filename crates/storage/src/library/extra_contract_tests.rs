//! Post-freeze library witnesses.
//!
//! `contract_tests.rs` is frozen byte-for-byte; new witnesses are added here
//! instead so the oracle keeps its recorded content and the additions stay
//! distinguishable from it.

#[allow(dead_code)]
#[path = "../../../archive/tests/fixtures/mod.rs"]
mod zip_fixture;

// Keep the named witnesses discoverable by `cargo test --lib extra_contract_tests`.
mod library_contract {
    use std::path::Path;

    use marduk_core::ids::{MemberId, ProfileId, ProfileRevision};
    use marduk_core::import::{
        BeginImport, ImportId, ImportTarget, NamedProfile, PreparedImport, ProfileEditError,
    };
    use marduk_core::profiles::ProfileSnapshot;
    use marduk_core::targets::RootRole;
    use sha2::{Digest, Sha256};

    use super::super::Catalog;
    use super::zip_fixture::{ClassicZip, Record};

    const FILE: &str = "textures/synthetic.dds";
    const A: &[u8] = b"old revision bytes";
    const B: &[u8] = b"new revision bytes";

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
        let prepared = catalog
            .prepare_import(archive.as_slice(), plan(revision, target))
            .expect("valid fixture ZIP must prepare");
        assert_eq!(prepared.profile, profile());
        assert_eq!(prepared.expected_revision, ProfileRevision::new(revision));
        let sha: [u8; 32] = Sha256::digest(&archive).into();
        assert_eq!(prepared.archive_sha256, sha);
        assert_eq!(prepared.files.len(), 1);
        assert_eq!(prepared.files[0].root, RootRole::Data);
        assert_eq!(prepared.files[0].relative.as_str(), FILE);
        prepared
    }

    fn assert_snapshot(
        snapshot: &ProfileSnapshot,
        revision: u64,
        members: &[(MemberId, ImportId, bool)],
    ) {
        assert_eq!(snapshot.id, profile());
        assert_eq!(snapshot.revision.get(), revision);
        assert_eq!(snapshot.members.len(), members.len());
        for (actual, &(id, installed, enabled)) in snapshot.members.iter().zip(members) {
            assert_eq!(actual.id, id);
            assert_eq!(actual.installed.get(), installed.get());
            assert_eq!(actual.enabled, enabled);
        }
    }

    fn assert_named(
        named: NamedProfile,
        revision: u64,
        members: &[(MemberId, ImportId, bool, &str)],
    ) {
        let expected: Vec<_> = members
            .iter()
            .map(|&(id, rev, en, _)| (id, rev, en))
            .collect();
        assert_snapshot(&named.snapshot, revision, &expected);
        assert_eq!(named.names.len(), members.len());
        for &(id, _, _, name) in members {
            assert_eq!(named.names.get(&id).map(String::as_str), Some(name));
        }
    }

    fn publish_a(catalog: &mut Catalog) -> (ImportId, MemberId) {
        let a = prepare(catalog, 0, append("A"), A).id;
        let snapshot = catalog.publish_import(a).unwrap();
        assert_eq!(snapshot.members.len(), 1);
        let member = snapshot.members[0].id;
        assert_snapshot(&snapshot, 1, &[(member, a, true)]);
        (a, member)
    }

    #[cfg(unix)]
    #[test]
    fn revisions_directory_is_private_to_the_owner() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(tmp.path()).unwrap();
        let revisions = tmp.path().join("revisions");
        assert!(revisions.is_dir());
        assert_eq!(
            revisions.metadata().unwrap().permissions().mode() & 0o777,
            0o700
        );
        drop(catalog);

        Catalog::open(tmp.path()).unwrap();
        assert_eq!(
            revisions.metadata().unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn reorder_named_returns_names_from_the_same_transaction() {
        let tmp = tempfile::tempdir().unwrap();
        let mut catalog = Catalog::open_for_test(tmp.path()).unwrap();
        let (a, member_a) = publish_a(&mut catalog);
        let b = prepare(&mut catalog, 1, append("B"), B).id;
        catalog.publish_import(b).unwrap();
        let snapshot = catalog.snapshot(profile()).unwrap();
        let member_b = snapshot.members[1].id;

        let named = catalog
            .reorder_named(profile(), ProfileRevision::new(2), &[member_b, member_a])
            .unwrap();
        assert_named(
            named,
            3,
            &[(member_b, b, true, "B"), (member_a, a, true, "A")],
        );

        let stale = catalog
            .reorder_named(profile(), ProfileRevision::new(2), &[member_b, member_a])
            .unwrap_err();
        assert_eq!(stale, ProfileEditError::StalePlan);
    }
}
