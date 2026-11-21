//! Implementer-owned Windows root tests.

#![cfg(windows)]

use std::fs;

use marduk_core::root_identity::{GameLayout, RootError};
use marduk_core::targets::RelativePath;
use marduk_platform::{bind_installation, observe_target, resolve_existing_parent};

const LAYOUT: GameLayout = GameLayout {
    data: "Data",
    markers: &["fixture.marker"],
};

#[test]
fn ancestor_rename_does_not_authorize_observation() {
    let temp = tempfile::tempdir().expect("scratch");
    let root = temp.path().join("root");
    let install = root.join("game");
    let data = install.join("Data");
    fs::create_dir_all(&data).expect("Data directory");
    fs::write(install.join("fixture.marker"), b"inert marker").expect("marker");
    fs::write(data.join("original.dds"), b"original bytes").expect("fixture file");

    let binding = bind_installation(&install, &LAYOUT).expect("bind installation");
    let target = resolve_existing_parent(&binding, &RelativePath::parse("original.dds").unwrap())
        .expect("resolve existing target");

    let moved = temp.path().join("moved");
    fs::rename(&root, &moved).expect("rename an ancestor above the installation");

    match observe_target(&target) {
        Err(RootError::TargetUnavailable) | Err(RootError::OutsideBoundary) => {}
        other => panic!("a stale locator must refuse, got {other:?}"),
    }
    assert_eq!(
        fs::read(moved.join("game/Data/original.dds")).unwrap(),
        b"original bytes"
    );
}
