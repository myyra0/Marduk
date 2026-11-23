//! Installation lease primitives: key stability and record arbitration.

use std::fs;
use std::path::{Path, PathBuf};

use marduk_platform::effects::directory_identity;
use marduk_platform::lease::{InstallationLease, LeaseClaim, LeaseError, LeaseRecord};
use marduk_platform::{WriterGate, open_private_directory};
use tempfile::TempDir;

fn lease_key(identity_key: [u8; 32]) -> String {
    let mut text = String::with_capacity(64);
    for byte in identity_key {
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

fn record(root: &str, reference: Option<&str>) -> LeaseRecord {
    LeaseRecord {
        app_data: PathBuf::from(root),
        reference: reference.map(str::to_owned),
    }
}

fn scratch() -> TempDir {
    tempfile::tempdir().unwrap()
}

#[test]
fn the_key_is_stable_across_reopen_and_distinct_for_siblings() {
    let temp = scratch();
    let first = temp.path().join("install-a");
    let second = temp.path().join("install-b");
    fs::create_dir_all(&first).unwrap();
    fs::create_dir_all(&second).unwrap();

    let first = directory_identity(&first).unwrap();
    let reopened = directory_identity(temp.path().join("install-a").as_path()).unwrap();
    let sibling = directory_identity(&second).unwrap();

    assert_eq!(
        first.installation_key(),
        reopened.installation_key(),
        "reopening the same directory gives the same key"
    );
    assert_ne!(
        first.installation_key(),
        sibling.installation_key(),
        "sibling installations get different keys"
    );
    assert_ne!(
        lease_key(first.installation_key()),
        lease_key(sibling.installation_key())
    );
}

#[test]
fn a_foreign_record_refuses_and_only_its_owner_releases_it() {
    let temp = scratch();
    let leases = temp.path().join("state").join("marduk").join("leases");
    let key = lease_key([9u8; 32]);

    let lease = InstallationLease::open(&leases, &key).unwrap();
    assert_eq!(lease.read().unwrap(), None, "a fresh lease is vacant");

    let owner = record("/data/owner", Some("plan:7"));
    assert_eq!(lease.claim(&owner).unwrap(), LeaseClaim::Acquired);
    assert_eq!(lease.read().unwrap(), Some(owner.clone()));

    // A second object over the same directory reads the same record.
    let reopened = InstallationLease::open(&leases, &key).unwrap();
    assert_eq!(reopened.read().unwrap(), Some(owner.clone()));
    let foreign = record("/data/other", Some("plan:8"));
    assert_eq!(
        reopened.claim(&foreign).unwrap(),
        LeaseClaim::Foreign(owner.app_data.clone())
    );
    assert_eq!(
        reopened.read().unwrap(),
        Some(owner.clone()),
        "a refused claim writes nothing"
    );

    assert!(!reopened.release(&foreign.app_data).unwrap());
    assert_eq!(reopened.read().unwrap(), Some(owner.clone()));
    assert!(reopened.release(&owner.app_data).unwrap());
    assert_eq!(reopened.read().unwrap(), None);
}

#[test]
fn an_unparseable_record_is_a_typed_refusal_not_absence() {
    let temp = scratch();
    let leases = temp.path().join("marduk").join("leases");
    let key = lease_key([3u8; 32]);
    let lease = InstallationLease::open(&leases, &key).unwrap();
    lease.claim(&record("/data/owner", None)).unwrap();
    let path = leases.join(format!("{key}.lease"));

    for torn in [
        b"garbage".as_slice(),
        b"marduk-lease/1\nzz\n-\n".as_slice(),
        b"marduk-lease/1\n2f\n-\nextra".as_slice(),
        b"marduk-lease/2\n2f\n-\n".as_slice(),
    ] {
        fs::write(&path, torn).unwrap();
        assert_eq!(lease.read(), Err(LeaseError::Unreadable), "{torn:?}");
        assert_eq!(
            lease.claim(&record("/data/other", None)),
            Err(LeaseError::Unreadable),
            "a torn record is never read as vacant"
        );
        assert_eq!(
            lease.release(&PathBuf::from("/data/owner")),
            Err(LeaseError::Unreadable)
        );
    }
}

#[test]
fn an_empty_record_is_unreadable_and_is_never_taken_as_vacant() {
    let temp = scratch();
    let leases = temp.path().join("marduk").join("leases");
    let key = lease_key([8u8; 32]);
    let lease = InstallationLease::open(&leases, &key).unwrap();
    let owner = record("/data/owner", Some("generation:1"));
    lease.claim(&owner).unwrap();
    let path = leases.join(format!("{key}.lease"));

    // The exact shape a truncate-then-write cut leaves behind.
    fs::write(&path, b"").unwrap();
    assert_eq!(lease.read(), Err(LeaseError::Unreadable));
    assert_eq!(
        lease.claim(&record("/data/other", None)),
        Err(LeaseError::Unreadable),
        "a crash-shaped record is never read as vacant"
    );
    assert_eq!(
        lease.clear_if(&owner.app_data, Some("generation:1")),
        Err(LeaseError::Unreadable)
    );
    assert_eq!(
        fs::read(&path).unwrap(),
        b"",
        "the crash shape is preserved for inspection"
    );
}

#[test]
fn an_empty_directory_has_no_record_and_is_not_created() {
    let temp = scratch();
    let leases = temp.path().join("absent").join("leases");
    assert!(
        InstallationLease::open_existing(&leases, &lease_key([1u8; 32]))
            .unwrap()
            .is_none()
    );
    assert!(!leases.exists(), "inspection creates nothing");
}

#[test]
fn a_key_that_is_not_a_digest_is_refused() {
    let temp = scratch();
    let leases = temp.path().join("leases");
    assert_eq!(
        InstallationLease::open(&leases, "../escape").err(),
        Some(LeaseError::Unreadable)
    );
    assert_eq!(
        InstallationLease::open(&leases, &"0".repeat(63)).err(),
        Some(LeaseError::Unreadable)
    );
}

#[test]
fn a_held_gate_is_busy_on_the_record_and_never_an_io_failure() {
    let temp = scratch();
    let leases = temp.path().join("marduk").join("leases");
    let key = lease_key([7u8; 32]);
    let lease = InstallationLease::open(&leases, &key).unwrap();
    let lock = format!("{key}.lock");

    // Vacant record: a held gate reports Busy, and the record is still absent
    // once the gate drops.
    {
        let dir = open_private_directory(&leases).unwrap();
        let gate = WriterGate::open_at(&dir, &lock).unwrap();
        let guard = gate.try_lock().unwrap();
        assert_eq!(lease.read(), Err(LeaseError::Busy));
        drop(guard);
    }
    assert_eq!(lease.read().unwrap(), None, "a vacant record stays vacant");

    // Claimed record: the same held gate still reads as Busy, not Io, and the
    // record is unchanged after the gate drops. On Windows the old design
    // returned Io here because the lock and the data shared one file.
    let owner = record("/data/owner", Some("plan:1"));
    assert_eq!(lease.claim(&owner).unwrap(), LeaseClaim::Acquired);
    {
        let dir = open_private_directory(&leases).unwrap();
        let gate = WriterGate::open_at(&dir, &lock).unwrap();
        let guard = gate.try_lock().unwrap();
        assert_eq!(lease.read(), Err(LeaseError::Busy));
        drop(guard);
    }
    assert_eq!(lease.read().unwrap(), Some(owner));
}

#[test]
fn a_torn_record_refuses_and_a_non_owner_release_leaves_it_intact() {
    let temp = scratch();
    let leases = temp.path().join("marduk").join("leases");
    let key = lease_key([4u8; 32]);
    let lease = InstallationLease::open(&leases, &key).unwrap();
    let owner = record("/data/owner", Some("plan:2"));
    lease.claim(&owner).unwrap();

    assert!(
        !lease.release(&PathBuf::from("/data/other")).unwrap(),
        "only the owner releases"
    );
    assert_eq!(
        lease.read().unwrap(),
        Some(owner),
        "a non-owner release leaves the record intact"
    );

    let path = leases.join(format!("{key}.lease"));
    let torn = b"marduk-lease/1\ntorn";
    fs::write(&path, torn).unwrap();
    assert_eq!(lease.read(), Err(LeaseError::Unreadable));
    assert_eq!(
        fs::read(&path).unwrap(),
        torn,
        "a refused read leaves the torn bytes for inspection"
    );
}

#[test]
fn clear_if_needs_the_exact_reference_and_never_clears_a_newer_claim() {
    let temp = scratch();
    let leases = temp.path().join("marduk").join("leases");
    let key = lease_key([10u8; 32]);
    let lease = InstallationLease::open(&leases, &key).unwrap();
    let older = record("/data/owner", Some("plan:1"));
    let newer = record("/data/owner", Some("plan:2"));
    lease.claim(&older).unwrap();

    assert!(
        !lease
            .clear_if(Path::new("/data/other"), Some("plan:1"))
            .unwrap(),
        "another catalog never clears this record"
    );
    assert!(
        !lease
            .clear_if(Path::new("/data/owner"), Some("plan:0"))
            .unwrap(),
        "a different reference is not this claim"
    );
    assert_eq!(lease.read().unwrap(), Some(older.clone()));

    lease.claim(&newer).unwrap();
    assert!(
        !lease
            .clear_if(Path::new("/data/owner"), Some("plan:1"))
            .unwrap(),
        "an older call cannot clear a newer claim"
    );
    assert_eq!(lease.read().unwrap(), Some(newer.clone()));
    assert!(
        lease
            .clear_if(Path::new("/data/owner"), Some("plan:2"))
            .unwrap(),
        "the matching claim clears"
    );
    assert_eq!(lease.read().unwrap(), None);

    // `None` matches only a record written without a reference.
    let bare = record("/data/owner", None);
    lease.claim(&bare).unwrap();
    assert!(
        !lease
            .clear_if(Path::new("/data/owner"), Some("plan:2"))
            .unwrap()
    );
    assert_eq!(lease.read().unwrap(), Some(bare.clone()));
    assert!(lease.clear_if(Path::new("/data/owner"), None).unwrap());
    assert_eq!(lease.read().unwrap(), None);
}

#[test]
fn a_claim_publishes_a_whole_record_and_consumes_its_temporary() {
    let temp = scratch();
    let leases = temp.path().join("marduk").join("leases");
    let key = lease_key([11u8; 32]);
    let writer = InstallationLease::open(&leases, &key).unwrap();
    let reader = InstallationLease::open(&leases, &key).unwrap();
    let path = leases.join(format!("{key}.lease"));
    let temporary = leases.join(format!("{key}.lease.tmp"));

    let first = record("/data/owner", Some("generation:1"));
    writer.claim(&first).unwrap();
    assert_eq!(reader.read().unwrap(), Some(first));
    assert!(!fs::read(&path).unwrap().is_empty());

    for generation in 2..=8 {
        let reference = format!("generation:{generation}");
        let next = record("/data/owner", Some(&reference));
        writer.claim(&next).unwrap();
        assert_eq!(
            reader.read().unwrap(),
            Some(next),
            "the other handle only ever sees a whole record"
        );
        assert!(
            !fs::read(&path).unwrap().is_empty(),
            "no empty intermediate remains after the claim"
        );
    }

    // A leftover from a cut before the replace is not a record and a later
    // claim removes it.
    fs::write(&temporary, b"stale leftover").unwrap();
    let last = record("/data/owner", Some("generation:9"));
    writer.claim(&last).unwrap();
    assert_eq!(reader.read().unwrap(), Some(last));
    assert!(!temporary.exists(), "the later claim cleans the temporary");
}

#[test]
fn a_failed_publication_leaves_the_previous_record_intact() {
    let temp = scratch();
    let leases = temp.path().join("marduk").join("leases");
    let key = lease_key([12u8; 32]);
    let lease = InstallationLease::open(&leases, &key).unwrap();
    let owner = record("/data/owner", Some("generation:1"));
    lease.claim(&owner).unwrap();

    // A directory squatting on the temporary name means no fresh sibling can be
    // created, so the replace is never reached.
    fs::create_dir(leases.join(format!("{key}.lease.tmp"))).unwrap();
    assert_eq!(
        lease.claim(&record("/data/owner", Some("generation:2"))),
        Err(LeaseError::Io)
    );
    assert_eq!(
        lease.read().unwrap(),
        Some(owner),
        "the previous record is still whole"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_lease_directory_with_group_access_is_refused() {
    use std::os::unix::fs::PermissionsExt;

    let temp = scratch();
    let leases = temp.path().join("leases");
    fs::create_dir_all(&leases).unwrap();
    fs::set_permissions(&leases, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        InstallationLease::open(&leases, &lease_key([5u8; 32])).err(),
        Some(LeaseError::Unreadable)
    );
}

#[cfg(target_os = "linux")]
#[test]
fn an_unreadable_lease_directory_is_not_absent() {
    use std::os::unix::fs::PermissionsExt;

    if unsafe { libc::geteuid() } == 0 {
        eprintln!("not run: root bypasses the directory permission check");
        return;
    }
    let temp = scratch();
    let leases = temp.path().join("leases");
    fs::create_dir_all(&leases).unwrap();
    fs::set_permissions(&leases, fs::Permissions::from_mode(0o000)).unwrap();
    let result = InstallationLease::open_existing(&leases, &lease_key([6u8; 32]));
    fs::set_permissions(&leases, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        result.err(),
        Some(LeaseError::Io),
        "an unreadable directory is a refusal, never an absent record"
    );
}
