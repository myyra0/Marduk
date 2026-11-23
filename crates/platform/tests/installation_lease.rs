//! Installation lease primitives: key stability and record arbitration.

use std::fs;
use std::path::PathBuf;

use marduk_platform::effects::directory_identity;
use marduk_platform::lease::{InstallationLease, LeaseClaim, LeaseError, LeaseRecord};
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

    // A zero-length file is the cleared form and reads as vacant.
    fs::write(&path, b"").unwrap();
    assert_eq!(lease.read().unwrap(), None);
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
