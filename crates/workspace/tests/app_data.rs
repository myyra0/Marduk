//! One resolver path for both clients, including Linux XDG fallback.

use std::path::Path;

use marduk_workspace::{APP_IDENTIFIER, app_data_root, app_data_root_in};

#[cfg(target_os = "linux")]
#[test]
fn relative_xdg_data_home_is_ignored_consistently() {
    let home = tempfile::tempdir().unwrap();
    // This binary has no other tests, so mutating the process environment here
    // cannot race a concurrent reader.
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::set_var("XDG_DATA_HOME", "relative/data");
    }

    let root = app_data_root().expect("HOME supplies the fallback");
    let fallback = home.path().join(".local/share");
    assert_eq!(root, app_data_root_in(&fallback));
    assert_eq!(root.file_name().unwrap(), APP_IDENTIFIER);

    let absolute = home.path().join("absolute-data");
    unsafe { std::env::set_var("XDG_DATA_HOME", &absolute) };
    assert_eq!(app_data_root().unwrap(), app_data_root_in(&absolute));

    unsafe {
        std::env::remove_var("XDG_DATA_HOME");
        std::env::remove_var("HOME");
    }
}

#[test]
fn the_identifier_is_joined_once() {
    let base = Path::new("/var/lib/user-data");
    assert_eq!(app_data_root_in(base), base.join(APP_IDENTIFIER));
}
