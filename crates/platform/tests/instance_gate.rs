//! One-client instance gate witnesses.
//!
//! Each test points the gate at its own temporary per-user root, so the
//! witnesses never contend with a Marduk client on the test machine. Env
//! mutation and the gate itself are process-global, so the tests serialize.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use marduk_platform::{GateError, acquire_instance_guard};

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Point the per-user instance root at `base` and return the previous value.
fn point_instance_root(base: &Path) -> (OsString, OsString) {
    #[cfg(target_os = "linux")]
    const KEY: &str = "XDG_RUNTIME_DIR";
    #[cfg(windows)]
    const KEY: &str = "LOCALAPPDATA";

    let previous = std::env::var_os(KEY).unwrap_or_default();
    // Safety: every env-mutating witness holds `SERIAL`.
    unsafe { std::env::set_var(KEY, base) };
    (KEY.into(), previous)
}

fn restore_instance_root(key: OsString, previous: OsString) {
    let key = key.to_string_lossy().into_owned();
    if previous.is_empty() {
        // Safety: every env-mutating witness holds `SERIAL`.
        unsafe { std::env::remove_var(key) };
    } else {
        // Safety: every env-mutating witness holds `SERIAL`.
        unsafe { std::env::set_var(key, previous) };
    }
}

fn expected_lock(base: &Path) -> PathBuf {
    base.join("marduk").join("instance.lock")
}

#[test]
fn guard_is_send_and_sync_for_managed_state() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<marduk_platform::InstanceGuard>();
}

#[test]
fn second_guard_in_one_process_is_busy() {
    let _serial = serial();
    let temp = tempfile::tempdir().unwrap();
    let key = point_instance_root(temp.path());

    let first = acquire_instance_guard().expect("first guard");
    assert!(matches!(acquire_instance_guard(), Err(GateError::Busy)));
    drop(first);
    let _second = acquire_instance_guard().expect("dropping the first guard releases the gate");

    restore_instance_root(key.0, key.1);
}

#[test]
fn dropping_the_guard_allows_a_new_one() {
    let _serial = serial();
    let temp = tempfile::tempdir().unwrap();
    let key = point_instance_root(temp.path());

    let first = acquire_instance_guard().expect("first guard");
    assert!(matches!(acquire_instance_guard(), Err(GateError::Busy)));
    drop(first);
    let _reacquired = acquire_instance_guard().expect("gate is free after the guard drops");

    restore_instance_root(key.0, key.1);
}

#[test]
fn lock_lives_beneath_the_per_user_root_not_a_catalog_root() {
    let _serial = serial();
    let temp = tempfile::tempdir().unwrap();
    let catalog = temp.path().join("catalog");
    fs::create_dir_all(&catalog).unwrap();
    let key = point_instance_root(temp.path());

    let _guard = acquire_instance_guard().expect("instance guard");
    let lock = expected_lock(temp.path());
    assert!(lock.is_file(), "{} is not a file", lock.display());
    assert!(!lock.starts_with(&catalog));

    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(temp.path().join("marduk"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
    }

    restore_instance_root(key.0, key.1);
}
