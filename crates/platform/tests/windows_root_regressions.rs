#![cfg(windows)]

use std::{fs, os::windows::io::AsRawHandle, path::Path, process::Command};

use marduk_core::root_identity::{GameLayout, RootError, TargetObservation, UnsupportedReason};
use marduk_core::targets::RelativePath;
use marduk_platform::{
    bind_installation, observe_target, reopen_installation, resolve_existing_parent,
};
use windows_sys::Wdk::Storage::FileSystem::NtSetEaFile;
use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

const LAYOUT: GameLayout = GameLayout {
    data: "Data",
    markers: &["fixture.marker"],
};

fn layout(install: &Path) {
    fs::create_dir_all(install.join("Data")).unwrap();
    fs::write(install.join("fixture.marker"), b"inert").unwrap();
}

#[test]
fn selected_installation_cannot_traverse_a_junction_ancestor() {
    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("real/game");
    layout(&install);
    let status = Command::new("cmd.exe")
        .current_dir(tmp.path())
        .args(["/d", "/c", "mklink", "/J", "alias", "real"])
        .status()
        .unwrap();
    assert!(status.success(), "fixture junction creation failed");
    let alias = tmp.path().join("alias/game");
    assert!(alias.join("fixture.marker").is_file());
    let binding = bind_installation(&install, &LAYOUT).unwrap();
    let (root, data) = binding.identities();
    assert!(matches!(
        bind_installation(&alias, &LAYOUT),
        Err(RootError::TargetUnavailable | RootError::OutsideBoundary)
    ));
    assert!(matches!(
        reopen_installation(&alias, &LAYOUT, root, data),
        Err(RootError::TargetUnavailable)
    ));
}

#[test]
fn intermediate_directory_rename_is_pinned_or_observation_refuses() {
    let tmp = tempfile::tempdir().unwrap();
    layout(tmp.path());
    let ancestor = tmp.path().join("Data/Textures");
    fs::create_dir_all(ancestor.join("Sub")).unwrap();
    let binding = bind_installation(tmp.path(), &LAYOUT).unwrap();
    let target = resolve_existing_parent(
        &binding,
        &RelativePath::parse("Textures/Sub/missing.dds").unwrap(),
    )
    .unwrap();
    match fs::rename(&ancestor, tmp.path().join("Data/Moved")) {
        Ok(()) => assert_eq!(observe_target(&target), Err(RootError::TargetUnavailable)),
        Err(error) => {
            assert!(matches!(error.raw_os_error(), Some(5 | 32)), "{error}");
            assert!(ancestor.join("Sub").is_dir());
            drop(target);
            drop(binding);
            fs::rename(&ancestor, tmp.path().join("Data/Moved"))
                .expect("rename must work once the pins are dropped");
        }
    }
}

#[test]
fn pinned_ancestor_refuses_a_write_handle() {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;

    let tmp = tempfile::tempdir().unwrap();
    let ancestor = tmp.path().join("root");
    let install = ancestor.join("game");
    layout(&install);
    let open_writer = || {
        fs::OpenOptions::new()
            .write(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(&ancestor)
    };
    drop(open_writer().expect("fixture permits a directory write handle"));
    let binding = bind_installation(&install, &LAYOUT).unwrap();
    let error = open_writer().expect_err("a pinned ancestor must deny write sharing");
    assert_eq!(error.raw_os_error(), Some(32));
    drop(binding);
    drop(open_writer().expect("write handle permitted once pins are dropped"));
}

#[test]
#[ignore = "requires NTFS scratch with 8.3 generation enabled; run explicitly on hosted Windows"]
fn short_name_alias_is_never_an_absent_or_admitted_leaf() {
    use std::os::windows::ffi::OsStrExt;

    let tmp = match std::env::var_os("MARDUK_M1_GAME_SCRATCH") {
        Some(dir) => tempfile::tempdir_in(dir).unwrap(),
        None => tempfile::tempdir().unwrap(),
    };
    layout(tmp.path());
    let path = tmp.path().join("Data/Original long filename.dds");
    fs::write(&path, b"inert").unwrap();
    let wide: Vec<_> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut short = [0u16; 2048];
    let len = unsafe { GetShortPathNameW(wide.as_ptr(), short.as_mut_ptr(), short.len() as u32) };
    assert!(
        len > 0 && (len as usize) < short.len(),
        "get native short path"
    );
    let short = String::from_utf16(&short[..len as usize]).unwrap();
    let alias = Path::new(&short).file_name().unwrap().to_str().unwrap();
    assert!(
        !alias.eq_ignore_ascii_case("Original long filename.dds"),
        "scratch must generate distinct 8.3 names"
    );
    assert_eq!(
        fs::read(tmp.path().join("Data").join(alias)).unwrap(),
        b"inert"
    );

    let binding = bind_installation(tmp.path(), &LAYOUT).unwrap();
    assert!(matches!(
        resolve_existing_parent(&binding, &RelativePath::parse(alias).unwrap()),
        Err(RootError::AmbiguousName)
    ));
    fs::remove_file(&path).unwrap();
    let relative = RelativePath::parse(alias).unwrap();
    let target = resolve_existing_parent(&binding, &relative).unwrap();
    fs::write(path, b"inert").unwrap();
    assert_eq!(
        fs::read(tmp.path().join("Data").join(alias)).unwrap(),
        b"inert"
    );
    assert_eq!(observe_target(&target), Err(RootError::AmbiguousName));
}

#[test]
fn writer_sharing_refuses_observation() {
    let tmp = tempfile::tempdir().unwrap();
    layout(tmp.path());
    let file = tmp.path().join("Data/original.dds");
    fs::write(&file, b"inert").unwrap();
    let binding = bind_installation(tmp.path(), &LAYOUT).unwrap();
    let target =
        resolve_existing_parent(&binding, &RelativePath::parse("original.dds").unwrap()).unwrap();
    let writer = fs::OpenOptions::new().write(true).open(&file).unwrap();
    assert_eq!(
        observe_target(&target),
        Ok(TargetObservation::Unsupported {
            reason: UnsupportedReason::Sharing
        })
    );
    drop(writer);
}

#[test]
fn ordinary_file_is_regular_without_security_privileges() {
    let tmp = tempfile::tempdir().unwrap();
    layout(tmp.path());
    let path = tmp.path().join("Data/original.dds");
    fs::write(&path, b"inert").unwrap();
    let binding = bind_installation(tmp.path(), &LAYOUT).unwrap();
    let target =
        resolve_existing_parent(&binding, &RelativePath::parse("original.dds").unwrap()).unwrap();
    let observation = observe_target(&target).unwrap();
    assert!(
        matches!(&observation, TargetObservation::Regular { fingerprint } if fingerprint.len == 5),
        "expected an ordinary original to be regular, got {observation:?}"
    );
}

#[test]
fn native_extended_attributes_refuse_an_original() {
    let tmp = tempfile::tempdir().unwrap();
    layout(tmp.path());
    let path = tmp.path().join("Data/original.dds");
    fs::write(&path, b"inert").unwrap();
    let binding = bind_installation(tmp.path(), &LAYOUT).unwrap();
    let target =
        resolve_existing_parent(&binding, &RelativePath::parse("original.dds").unwrap()).unwrap();
    let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
    // FILE_FULL_EA_INFORMATION with name "TEST" and one byte value.
    #[repr(align(4))]
    struct AlignedEa([u8; 14]);
    let ea = AlignedEa([0, 0, 0, 0, 0, 4, 1, 0, b'T', b'E', b'S', b'T', 0, 42]);
    let mut io = IO_STATUS_BLOCK::default();
    assert_eq!(
        unsafe {
            NtSetEaFile(
                file.as_raw_handle(),
                &mut io,
                ea.0.as_ptr().cast(),
                ea.0.len() as u32,
            )
        },
        0
    );
    drop(file);

    assert_eq!(
        observe_target(&target),
        Ok(TargetObservation::Unsupported {
            reason: UnsupportedReason::Metadata
        })
    );
}
