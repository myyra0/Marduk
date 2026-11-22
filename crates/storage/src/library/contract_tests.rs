#[allow(dead_code)]
#[path = "../../../archive/tests/fixtures/mod.rs"]
mod zip_fixture;

// Keep the named witnesses discoverable by `cargo test --lib library_contract`.
mod library_contract {
    use std::path::{Path, PathBuf};

    use marduk_core::ids::{MemberId, ProfileId, ProfileRevision};
    use marduk_core::import::{
        ArchiveError, BeginImport, ImportError, ImportId, ImportTarget, NamedProfile,
        PreparedImport, ProfileEditError,
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

    fn payload(root: &Path, id: ImportId) -> PathBuf {
        root.join("revisions")
            .join(id.get().to_string())
            .join("payload")
            .join(FILE)
    }

    fn assert_payload(root: &Path, id: ImportId, bytes: &[u8]) {
        assert_eq!(std::fs::read(payload(root, id)).unwrap(), bytes);
    }

    fn sql_id(id: ImportId) -> i64 {
        i64::try_from(id.get()).unwrap()
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

    fn state(catalog: &Catalog, id: ImportId) -> (String, Option<i64>) {
        catalog
            .connection
            .query_row(
                "SELECT state, result_revision FROM imports WHERE id = ?1",
                [sql_id(id)],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
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
    fn failed_reinstall_keeps_previous_revision_and_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mut catalog = Catalog::open_for_test(root).unwrap();
        let (a, member) = publish_a(&mut catalog);
        let b = prepare(&mut catalog, 1, ImportTarget::Replace { member }, B).id;
        assert_ne!(a, b);
        assert_eq!(state(&catalog, b), ("prepared".into(), None));
        assert_named(
            catalog.named_profile(profile()).unwrap(),
            1,
            &[(member, a, true, "A")],
        );
        assert_payload(root, b, B);

        drop(catalog);
        let mut catalog = Catalog::open_for_test(root).unwrap();
        assert_named(
            catalog.named_profile(profile()).unwrap(),
            1,
            &[(member, a, true, "A")],
        );
        assert_eq!(published(&catalog), [sql_id(a)]);
        assert_eq!(state(&catalog, b), ("prepared".into(), None));
        assert_payload(root, a, A);
    }

    #[test]
    fn stale_install_preview_publishes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mut catalog = Catalog::open_for_test(root).unwrap();
        let (a, member) = publish_a(&mut catalog);
        for revision in 1..8 {
            let snapshot = catalog
                .reorder(profile(), ProfileRevision::new(revision), &[member])
                .unwrap();
            assert_snapshot(&snapshot, revision + 1, &[(member, a, true)]);
        }
        let b = prepare(&mut catalog, 8, append("B"), B).id;
        let changed = catalog
            .reorder(profile(), ProfileRevision::new(8), &[member])
            .unwrap();
        assert_snapshot(&changed, 9, &[(member, a, true)]);
        assert_eq!(
            catalog.publish_import(b).unwrap_err(),
            ImportError::StalePlan
        );
        assert_named(
            catalog.named_profile(profile()).unwrap(),
            9,
            &[(member, a, true, "A")],
        );
        assert_eq!(published(&catalog), [sql_id(a)]);
        assert_eq!(state(&catalog, b), ("prepared".into(), None));
        assert_payload(root, a, A);
        assert_payload(root, b, B);
    }

    #[test]
    fn published_import_replay_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mut catalog = Catalog::open_for_test(root).unwrap();
        let (a, member_a) = publish_a(&mut catalog);
        drop(catalog);

        let mut catalog = Catalog::open_for_test(root).unwrap();
        assert_snapshot(
            &catalog.publish_import(a).unwrap(),
            1,
            &[(member_a, a, true)],
        );
        assert_named(
            catalog.named_profile(profile()).unwrap(),
            1,
            &[(member_a, a, true, "A")],
        );
        assert_eq!(published(&catalog), [sql_id(a)]);

        let b = prepare(&mut catalog, 1, append("B"), B).id;
        let changed = catalog.publish_import(b).unwrap();
        assert_eq!(changed.members.len(), 2);
        let member_b = changed.members[1].id;
        assert_ne!(member_a, member_b);
        assert_snapshot(
            &catalog.publish_import(a).unwrap(),
            1,
            &[(member_a, a, true)],
        );
        // Publish returns a ProfileSnapshot; recorded names live in the result rows.
        let (count, name): (i64, String) = catalog
            .connection
            .query_row(
                "SELECT count(*), min(display_name) FROM import_result_members WHERE import_id = ?1",
                [sql_id(a)],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((count, name.as_str()), (1, "A"));
        assert_eq!(state(&catalog, a), ("committed".into(), Some(1)));
        assert_named(
            catalog.named_profile(profile()).unwrap(),
            2,
            &[(member_a, a, true, "A"), (member_b, b, true, "B")],
        );
        assert_eq!(published(&catalog), [sql_id(a), sql_id(b)]);
        assert_payload(root, a, A);
        assert_payload(root, b, B);
    }

    #[test]
    fn reconciliation_never_reclaims_referenced_payload() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mut catalog = Catalog::open_for_test(root).unwrap();
        let (a, member) = publish_a(&mut catalog);
        let b = prepare(&mut catalog, 1, append("B"), B).id;
        let invalid = zip(B);
        let truncated = &invalid[..invalid.len() - 22]; // Missing EOCD.
        assert_eq!(
            catalog
                .prepare_import(truncated, plan(1, append("Abandoned")))
                .unwrap_err(),
            ImportError::Archive(ArchiveError::InvalidContainer),
        );
        let (id, count): (i64, i64) = catalog
            .connection
            .query_row(
                "SELECT max(id), count(*) FROM imports WHERE state = 'writing'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(count, 1);
        let abandoned = ImportId::try_from_u64(u64::try_from(id).unwrap()).unwrap();
        assert_ne!(abandoned, a);
        assert_ne!(abandoned, b);
        assert_eq!(published(&catalog), [sql_id(a)]);
        drop(catalog);

        let mut catalog = Catalog::open_for_test(root).unwrap();
        assert_eq!(catalog.reconcile().unwrap(), [b]);
        let (quarantined, result) = state(&catalog, abandoned);
        assert!(matches!(quarantined.as_str(), "writing" | "discarded"));
        assert_eq!(result, None);
        assert_eq!(
            catalog.publish_import(abandoned).unwrap_err(),
            ImportError::ImportNotPrepared
        );
        assert_eq!(state(&catalog, b), ("prepared".into(), None));
        assert_eq!(
            catalog.discard_import(a),
            Err(ImportError::ImportNotPrepared)
        );
        assert_eq!(state(&catalog, a), ("committed".into(), Some(1)));
        assert_eq!(published(&catalog), [sql_id(a)]);
        assert_named(
            catalog.named_profile(profile()).unwrap(),
            1,
            &[(member, a, true, "A")],
        );
        assert_payload(root, a, A);
        assert_payload(root, b, B);
    }

    #[test]
    fn append_and_replace_preserve_priority_and_immutable_revisions() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mut catalog = Catalog::open_for_test(root).unwrap();
        let (a, member_a) = publish_a(&mut catalog);
        let b = prepare(&mut catalog, 1, append("A"), B).id;
        let appended = catalog.publish_import(b).unwrap();
        assert_eq!(appended.members.len(), 2);
        let member_b = appended.members[1].id;
        assert_ne!(member_a, member_b);
        assert_snapshot(&appended, 2, &[(member_a, a, true), (member_b, b, true)]);
        // This surface has no toggle command. Seed a disabled row to distinguish
        // a true replacement from an update that resets the flag to enabled.
        assert_eq!(
            catalog
                .connection
                .execute(
                    "UPDATE members SET enabled = 0 WHERE id = ?1",
                    [i64::try_from(member_a.get()).unwrap()],
                )
                .unwrap(),
            1,
        );
        let ordinal = |catalog: &Catalog| -> i64 {
            catalog
                .connection
                .query_row(
                    "SELECT ordinal FROM members WHERE id = ?1",
                    [i64::try_from(member_a.get()).unwrap()],
                    |row| row.get(0),
                )
                .unwrap()
        };
        let before = ordinal(&catalog);
        let c = prepare(
            &mut catalog,
            2,
            ImportTarget::Replace { member: member_a },
            b"C",
        )
        .id;
        assert_ne!(a, c);
        assert_ne!(b, c);
        assert_eq!(published(&catalog), [sql_id(a), sql_id(b)]);
        let replaced = catalog.publish_import(c).unwrap();
        assert_snapshot(&replaced, 3, &[(member_a, c, false), (member_b, b, true)]);
        assert_eq!(ordinal(&catalog), before);
        assert_named(
            catalog.named_profile(profile()).unwrap(),
            3,
            &[(member_a, c, false, "A"), (member_b, b, true, "A")],
        );
        assert_eq!(published(&catalog), [sql_id(a), sql_id(b), sql_id(c)]);
        assert_payload(root, a, A);
        assert_payload(root, b, B);
        assert_payload(root, c, b"C");
    }

    #[test]
    fn reorder_requires_exact_permutation_and_advances_once() {
        let tmp = tempfile::tempdir().unwrap();
        let mut catalog = Catalog::open_for_test(tmp.path()).unwrap();
        let (a, member_a) = publish_a(&mut catalog);
        let b = prepare(&mut catalog, 1, append("B"), B).id;
        let snapshot = catalog.publish_import(b).unwrap();
        assert_eq!(snapshot.members.len(), 2);
        let member_b = snapshot.members[1].id;
        let at_two = ProfileRevision::new(2);
        let unknown = MemberId::new(u64::try_from(i64::MAX).unwrap());
        assert_ne!(unknown, member_a);
        assert_ne!(unknown, member_b);
        for order in [
            &[MemberId::new(0), member_b][..],
            &[member_a, unknown],
            &[member_a, member_a],
            &[member_a],
        ] {
            assert_eq!(
                catalog.reorder(profile(), at_two, order).unwrap_err(),
                ProfileEditError::InvalidOrder,
                "order {order:?}",
            );
            assert_snapshot(
                &catalog.snapshot(profile()).unwrap(),
                2,
                &[(member_a, a, true), (member_b, b, true)],
            );
        }
        let order = [member_b, member_a];
        let reversed = catalog.reorder(profile(), at_two, &order).unwrap();
        assert_snapshot(&reversed, 3, &[(member_b, b, true), (member_a, a, true)]);
        assert_eq!(
            catalog.reorder(profile(), at_two, &order).unwrap_err(),
            ProfileEditError::StalePlan,
        );
        let identical = catalog
            .reorder(profile(), ProfileRevision::new(3), &order)
            .unwrap();
        assert_snapshot(&identical, 4, &[(member_b, b, true), (member_a, a, true)]);
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

    #[test]
    fn names_are_trimmed_bounded_utf8_labels_not_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let mut catalog = Catalog::open_for_test(&root).unwrap();
        let too_long = format!("{}x", "é".repeat(60));
        let archive = zip(A);
        for invalid in ["", "   ", "A\0B", "A\nB", "A\u{0085}B", &too_long] {
            assert_eq!(
                catalog
                    .prepare_import(archive.as_slice(), plan(0, append(invalid)))
                    .unwrap_err(),
                ImportError::InvalidName,
                "label {invalid:?}",
            );
        }
        assert_named(catalog.named_profile(profile()).unwrap(), 0, &[]);
        assert!(published(&catalog).is_empty());
        let mut expected = Vec::new();
        let max = "é".repeat(60);
        for (revision, label) in [(0, "x"), (1, max.as_str()), (2, "../label-only/child")] {
            let id = prepare(&mut catalog, revision, append(&format!("  {label}  ")), A).id;
            let snapshot = catalog.publish_import(id).unwrap();
            assert_eq!(snapshot.members.len(), expected.len() + 1);
            expected.push((snapshot.members.last().unwrap().id, id, true, label));
            assert_named(
                catalog.named_profile(profile()).unwrap(),
                revision + 1,
                &expected,
            );
            assert_payload(&root, id, A);
        }
        assert!(!tmp.path().join("label-only").exists());
        assert!(!root.join("label-only").exists());
        let revision_dirs: Vec<_> = std::fs::read_dir(root.join("revisions"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(revision_dirs.len(), expected.len());
        for &(_, id, _, _) in &expected {
            assert!(revision_dirs.contains(&id.get().to_string().into()));
        }
    }

    #[test]
    fn changed_prepared_payload_is_never_published() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mut catalog = Catalog::open_for_test(root).unwrap();
        let a = prepare(&mut catalog, 0, append("A"), A).id;
        assert_named(catalog.named_profile(profile()).unwrap(), 0, &[]);
        assert!(published(&catalog).is_empty());
        assert_payload(root, a, A);
        let changed = vec![b'X'; A.len()];
        std::fs::write(payload(root, a), &changed).unwrap();
        assert_eq!(
            catalog.publish_import(a).unwrap_err(),
            ImportError::StageChanged
        );
        drop(catalog);

        let mut catalog = Catalog::open_for_test(root).unwrap();
        assert_named(catalog.named_profile(profile()).unwrap(), 0, &[]);
        assert!(published(&catalog).is_empty());
        let results: i64 = catalog
            .connection
            .query_row("SELECT count(*) FROM import_result_members", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(results, 0);
        assert_payload(root, a, &changed);
    }

    #[test]
    fn sqlite_pragmas_and_published_state_survive_reopen() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let assert_pragmas = |catalog: &Catalog| {
            let pragma = |name| {
                catalog
                    .connection
                    .pragma_query_value(None, name, |row| row.get::<_, i64>(0))
                    .unwrap()
            };
            let mode: String = catalog
                .connection
                .pragma_query_value(None, "journal_mode", |row| row.get(0))
                .unwrap();
            assert_eq!(pragma("foreign_keys"), 1);
            assert_eq!(mode, "wal");
            assert_eq!(pragma("synchronous"), 2);
        };
        let mut catalog = Catalog::open_for_test(root).unwrap();
        assert_pragmas(&catalog);
        assert!(root.join("catalog.sqlite3").is_file());
        assert_named(catalog.named_profile(profile()).unwrap(), 0, &[]);
        let (a, member) = publish_a(&mut catalog);
        drop(catalog);

        let mut catalog = Catalog::open_for_test(root).unwrap();
        assert_pragmas(&catalog);
        assert_named(
            catalog.named_profile(profile()).unwrap(),
            1,
            &[(member, a, true, "A")],
        );
        let (id, revision): (i64, i64) = catalog
            .connection
            .query_row(
                "SELECT id, result_revision FROM imports WHERE state = 'committed'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            ImportId::try_from_u64(u64::try_from(id).unwrap()).unwrap(),
            a
        );
        assert_eq!(u64::try_from(revision).unwrap(), 1);
        assert_payload(root, a, A);
    }

    #[test]
    fn resolved_inputs_distinguishes_unknown_from_empty_revisions() {
        use marduk_core::ids::InstalledRevisionId;

        let tmp = tempfile::tempdir().unwrap();
        let mut catalog = Catalog::open_for_test(tmp.path()).unwrap();
        let (a, _) = publish_a(&mut catalog);
        let installed = InstalledRevisionId::new(a.get());
        let inputs = catalog.resolved_inputs(profile()).unwrap();
        assert_eq!(
            inputs.manifests.get(&installed).map(Vec::len),
            Some(1),
            "a published revision with rows has a manifest"
        );

        catalog
            .connection
            .execute(
                "DELETE FROM revision_files WHERE revision_id = ?1",
                [sql_id(a)],
            )
            .unwrap();
        let inputs = catalog.resolved_inputs(profile()).unwrap();
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
            .execute("DELETE FROM installed_revisions WHERE id = ?1", [sql_id(a)])
            .unwrap();
        catalog
            .connection
            .execute("PRAGMA foreign_keys = ON", [])
            .unwrap();
        let inputs = catalog.resolved_inputs(profile()).unwrap();
        assert!(
            inputs.manifests.get(&installed).is_none(),
            "an unknown revision has no manifest at all"
        );
    }

    #[test]
    fn signed_sqlite_boundary_round_trips_without_revision_wrap() {
        let tmp = tempfile::tempdir().unwrap();
        let mut catalog = Catalog::open_for_test(tmp.path()).unwrap();
        let (a, member_a) = publish_a(&mut catalog);
        let near_max = i64::MAX - 1;
        // Reach the boundary without exhausting the sequence through public writes.
        assert_eq!(
            catalog
                .connection
                .execute(
                    "UPDATE sqlite_sequence SET seq = ?1 WHERE name = 'imports'",
                    [near_max],
                )
                .unwrap(),
            1
        );
        assert_eq!(
            catalog
                .connection
                .execute("UPDATE profiles SET revision = ?1 WHERE id = 1", [near_max],)
                .unwrap(),
            1
        );
        let max = u64::try_from(i64::MAX).unwrap();
        let b = prepare(&mut catalog, max - 1, append("B"), B).id;
        assert_eq!(b.get(), max);
        let snapshot = catalog.publish_import(b).unwrap();
        assert_eq!(snapshot.members.len(), 2);
        let member_b = snapshot.members[1].id;
        let members = [(member_a, a, true), (member_b, b, true)];
        assert_snapshot(&snapshot, max, &members);
        drop(catalog);

        let mut catalog = Catalog::open_for_test(tmp.path()).unwrap();
        assert_eq!(state(&catalog, b), ("committed".into(), Some(i64::MAX)));
        assert_eq!(published(&catalog), [sql_id(a), i64::MAX]);
        assert_snapshot(&catalog.publish_import(b).unwrap(), max, &members);
        assert_eq!(
            catalog
                .reorder(profile(), ProfileRevision::new(max), &[member_b, member_a])
                .unwrap_err(),
            ProfileEditError::Storage,
        );
        assert_snapshot(&catalog.snapshot(profile()).unwrap(), max, &members);
        assert_payload(tmp.path(), b, B);
    }
}
