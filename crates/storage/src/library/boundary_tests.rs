//! Focused catalog-root boundary tests. The frozen contract oracle remains in
//! `contract_tests.rs`.

use super::Catalog;
use marduk_core::import::ImportError;

#[cfg(unix)]
#[test]
fn catalog_open_refuses_a_redirected_app_root_without_writing() {
    let tmp = tempfile::tempdir().unwrap();
    let game_tree = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(game_tree.path(), tmp.path().join("app-data")).unwrap();

    let error = match Catalog::open(&tmp.path().join("app-data")) {
        Ok(_) => panic!("a symlinked app-data root must not be opened"),
        Err(error) => error,
    };
    assert_eq!(error, ImportError::Storage);
    assert_eq!(std::fs::read_dir(game_tree.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn catalog_open_refuses_a_redirected_root_ancestor_without_writing() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), tmp.path().join("link")).unwrap();

    let error = match Catalog::open(&tmp.path().join("link").join("app-data")) {
        Ok(_) => panic!("a redirected root ancestor must not be opened"),
        Err(error) => error,
    };
    assert_eq!(error, ImportError::Storage);
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn catalog_open_refuses_a_redirected_catalog_file_without_writing() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tmp.path().join("outside.sqlite3");
    std::fs::write(&outside, b"not a catalog").unwrap();
    std::os::unix::fs::symlink(&outside, tmp.path().join("catalog.sqlite3")).unwrap();

    let error = match Catalog::open(tmp.path()) {
        Ok(_) => panic!("a symlinked catalog file must not be opened"),
        Err(error) => error,
    };
    assert_eq!(error, ImportError::Storage);
    assert_eq!(std::fs::read(&outside).unwrap(), b"not a catalog");
}

#[cfg(unix)]
#[test]
fn catalog_open_refuses_symlinked_revisions_without_writing() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), tmp.path().join("revisions")).unwrap();

    let error = match Catalog::open(tmp.path()) {
        Ok(_) => panic!("a symlinked revisions tree must not be opened"),
        Err(error) => error,
    };
    assert_eq!(error, ImportError::Storage);
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn catalog_open_refuses_a_file_at_revisions_without_replacing_it() {
    let tmp = tempfile::tempdir().unwrap();
    let revisions = tmp.path().join("revisions");
    std::fs::write(&revisions, b"not a directory").unwrap();

    let error = match Catalog::open(tmp.path()) {
        Ok(_) => panic!("a non-directory revisions path must not be opened"),
        Err(error) => error,
    };
    assert_eq!(error, ImportError::Storage);
    assert_eq!(std::fs::read(&revisions).unwrap(), b"not a directory");
}
