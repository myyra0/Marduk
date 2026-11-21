#![cfg(any(target_os = "linux", windows))]

use std::fs;
use std::path::Path;

use marduk_core::root_identity::{GameLayout, RootError, UnsupportedReason};
use marduk_games::skyrim_se::LAYOUT;
use marduk_platform::bind_installation;

fn scratch() -> tempfile::TempDir {
    if let Some(parent) = std::env::var_os("MARDUK_M1_GAME_SCRATCH") {
        tempfile::tempdir_in(parent).unwrap()
    } else if cfg!(target_os = "linux") {
        // /tmp can be tmpfs, which is not a qualified installation filesystem.
        tempfile::tempdir_in("/var/tmp").unwrap()
    } else {
        tempfile::tempdir().unwrap()
    }
}

#[test]
fn layout_accepts_either_inert_marker_in_the_selected_installation() {
    let layout: &GameLayout = &LAYOUT;
    assert_eq!(layout.data, "Data");
    let scratch = scratch();
    for (name, marker) in [
        ("launcher", "SkyrimSELauncher.exe"),
        ("game", "SkyrimSE.exe"),
    ] {
        let install = scratch.path().join(name);
        fs::create_dir_all(install.join("Data")).unwrap();
        let marker = install.join(marker);
        const INERT: &[u8] = b"not an executable";
        fs::write(&marker, INERT).unwrap();

        let binding = bind_installation(&install, layout).expect("synthetic Skyrim SE layout");
        let (installation, data) = binding.identities();
        assert_ne!(installation, data);
        assert_eq!(fs::read(marker).unwrap(), INERT);
    }
}

#[test]
fn missing_installation_marker_is_a_typed_layout_refusal() {
    let scratch = scratch();
    let install: &Path = scratch.path();
    fs::create_dir(install.join("Data")).unwrap();
    let misplaced = install.join("Data/SkyrimSE.exe");
    fs::write(&misplaced, b"not an installation marker").unwrap();

    assert!(matches!(
        bind_installation(install, &LAYOUT),
        Err(RootError::UnsupportedTarget(
            UnsupportedReason::Configuration
        ))
    ));
    assert_eq!(fs::read(misplaced).unwrap(), b"not an installation marker");
}
