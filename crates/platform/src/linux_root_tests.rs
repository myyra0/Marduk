//! Implementer-owned Linux root tests.

use std::ffi::CString;
use std::fs;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use tempfile::TempDir;

use super::*;

/// Ordinary extent-mapped ext4 files carry this flag.
const FS_EXTENT_FL: libc::c_long = 0x0008_0000;

const LAYOUT: GameLayout = GameLayout {
    data: "Data",
    markers: &["fixture.marker"],
};

fn scratch() -> TempDir {
    if let Some(parent) = std::env::var_os("MARDUK_M1_GAME_SCRATCH") {
        tempfile::tempdir_in(parent).expect("scratch parent")
    } else {
        // The oracle uses the same non-tmpfs default.
        tempfile::tempdir_in("/var/tmp").expect("Btrfs/ext4 scratch parent")
    }
}

fn layout_at(install: &Path) {
    fs::create_dir_all(install.join("Data")).expect("Data");
    fs::write(install.join("fixture.marker"), b"inert marker").expect("marker");
}

#[test]
fn extent_mapped_flags_are_supported() {
    assert!(flags_are_supported(0));
    assert!(flags_are_supported(FS_EXTENT_FL));
}

#[test]
fn immutable_append_encrypt_verity_and_cow_flags_are_refused() {
    for flag in [
        FS_IMMUTABLE_FL,
        FS_APPEND_FL,
        FS_ENCRYPT_FL,
        FS_VERITY_FL,
        FS_NOCOW_FL,
        FS_DAX_FL,
    ] {
        assert!(!flags_are_supported(flag), "accepted {flag:#x}");
    }
}

#[test]
fn ordinary_user_modifiable_flags_are_refused() {
    const FS_SYNC_FL: libc::c_long = 0x0000_0008;
    const FS_NODUMP_FL: libc::c_long = 0x0000_0040;
    assert!(!flags_are_supported(FS_SYNC_FL));
    assert!(!flags_are_supported(FS_NODUMP_FL));
}

#[test]
fn existing_file_with_user_xattr_is_unsupported_metadata() {
    let directory = scratch();
    let install = directory.path().join("game");
    layout_at(&install);
    let file = install.join("Data/mod.dds");
    fs::write(&file, b"payload").expect("fixture file");

    let path = CString::new(file.as_os_str().as_bytes()).expect("path");
    let name = c"user.marduk_test";
    let value = b"x";
    let set = unsafe {
        libc::setxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    };
    if set != 0 {
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EOPNOTSUPP) | Some(libc::EPERM) => {
                eprintln!("not run: scratch filesystem refuses user xattrs: {error}");
                return;
            }
            _ => panic!("setting a test xattr failed: {error}"),
        }
    }

    let binding = bind_installation(&install, &LAYOUT).expect("bind installation");
    let target = resolve_existing_parent(&binding, &RelativePath::parse("mod.dds").unwrap())
        .expect("resolve existing leaf");
    assert!(matches!(
        observe_target(&target),
        Ok(TargetObservation::Unsupported {
            reason: UnsupportedReason::Metadata
        })
    ));
}

#[test]
fn existing_file_with_nocow_flag_is_unsupported_metadata_or_not_run() {
    // `_IOW('f', 2, long)` from `linux/fs.h`.
    const FS_IOC_SETFLAGS: libc::c_ulong = 0x4008_6602;

    let directory = scratch();
    let install = directory.path().join("game");
    layout_at(&install);
    let file = install.join("Data/nocow.dds");
    fs::File::create(&file).expect("empty fixture file");
    let handle = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&file)
        .expect("open fixture");

    let mut flags: libc::c_long = 0;
    if unsafe { libc::ioctl(handle.as_raw_fd(), FS_IOC_GETFLAGS, &mut flags) } != 0 {
        eprintln!(
            "not run: GETFLAGS unavailable: {}",
            std::io::Error::last_os_error()
        );
        return;
    }
    let wanted = flags | FS_NOCOW_FL;
    if unsafe { libc::ioctl(handle.as_raw_fd(), FS_IOC_SETFLAGS, &wanted) } != 0 {
        eprintln!(
            "not run: scratch filesystem refuses FS_NOCOW_FL: {}",
            std::io::Error::last_os_error()
        );
        return;
    }
    drop(handle);

    let binding = bind_installation(&install, &LAYOUT).expect("bind installation");
    let target = resolve_existing_parent(&binding, &RelativePath::parse("nocow.dds").unwrap())
        .expect("resolve existing leaf");
    assert!(matches!(
        observe_target(&target),
        Ok(TargetObservation::Unsupported {
            reason: UnsupportedReason::Metadata
        })
    ));
}
