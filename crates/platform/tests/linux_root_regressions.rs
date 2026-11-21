#![cfg(target_os = "linux")]

use std::{fs, os::unix::fs::symlink, path::Path};

use marduk_core::root_identity::{
    GameLayout, NativeIdentity, RootError, RootIdentity, TargetObservation, UnsupportedReason,
};
use marduk_core::targets::RelativePath;
use marduk_platform::{
    bind_installation, observe_target, reopen_installation, resolve_existing_parent,
};

const LAYOUT: GameLayout = GameLayout {
    data: "Data",
    markers: &["fixture.marker"],
};

#[test]
#[ignore = "requires two mount views exposing the same filesystem and objects"]
fn same_objects_on_another_mount_view_cannot_reopen() {
    let first = std::env::var_os("MARDUK_M1_VIEW_A").expect("first bind mount");
    let second = std::env::var_os("MARDUK_M1_VIEW_B").expect("second bind mount");
    let first = Path::new(&first);
    let second = Path::new(&second);
    let original = bind_installation(first, &LAYOUT).unwrap();
    let alternate = bind_installation(second, &LAYOUT).unwrap();
    let (root, data) = original.identities();
    let (other_root, other_data) = alternate.identities();
    for (left, right) in [(root, other_root), (data, other_data)] {
        let NativeIdentity::Linux { view_id, .. } = *left.evidence() else {
            panic!("expected Linux identity");
        };
        let mut otherwise_same = *right.evidence();
        let NativeIdentity::Linux {
            view_id: other_view_id,
            ..
        } = &mut otherwise_same
        else {
            panic!("expected Linux identity");
        };
        assert_ne!(view_id, *other_view_id);
        *other_view_id = view_id;
        assert_eq!(*left.evidence(), otherwise_same);
        assert_ne!(left, right);
    }
    let actual = reopen_installation(second, &LAYOUT, root, data);
    assert!(
        matches!(actual, Err(RootError::TargetUnavailable)),
        "same inode through another mount must refuse reopen: {actual:?}"
    );
    assert_eq!(
        reopen_installation(first, &LAYOUT, root, data)
            .unwrap()
            .identities(),
        (root, data)
    );
}

fn layout(install: &Path) {
    fs::create_dir_all(install.join("Data")).unwrap();
    fs::write(install.join("fixture.marker"), b"inert").unwrap();
}

#[test]
fn reopen_requires_exact_view_id_for_each_root() {
    let temp = tempfile::tempdir_in("/var/tmp").unwrap();
    layout(temp.path());
    let bound = bind_installation(temp.path(), &LAYOUT).unwrap();
    let (root, data) = bound.identities();
    let other_view = |identity: &RootIdentity| {
        let mut native = *identity.evidence();
        let NativeIdentity::Linux { view_id, .. } = &mut native else {
            panic!("expected Linux identity");
        };
        assert_ne!(*view_id, 0);
        *view_id ^= 1_u64 << 63;
        let other = RootIdentity::from_adapter_evidence(native);
        assert_ne!(&other, identity);
        other
    };
    assert!(matches!(
        reopen_installation(temp.path(), &LAYOUT, &other_view(root), data),
        Err(RootError::TargetUnavailable)
    ));
    assert!(matches!(
        reopen_installation(temp.path(), &LAYOUT, root, &other_view(data)),
        Err(RootError::TargetUnavailable)
    ));
    assert_eq!(
        reopen_installation(temp.path(), &LAYOUT, root, data)
            .unwrap()
            .identities(),
        (root, data)
    );
}

#[test]
fn selected_installation_cannot_traverse_a_symlink_ancestor() {
    let temp = tempfile::tempdir_in("/var/tmp").unwrap();
    let install = temp.path().join("real/game");
    layout(&install);
    let binding = bind_installation(&install, &LAYOUT).unwrap();
    let (root, data) = binding.identities();
    let alias = temp.path().join("alias");
    symlink(temp.path().join("real"), &alias).unwrap();
    let aliased = alias.join("game");
    assert!(matches!(
        bind_installation(&aliased, &LAYOUT),
        Err(RootError::TargetUnavailable)
    ));
    assert!(matches!(
        reopen_installation(&aliased, &LAYOUT, root, data),
        Err(RootError::TargetUnavailable)
    ));
}

#[test]
fn observation_finds_a_case_only_leaf_rename() {
    let temp = tempfile::tempdir_in("/var/tmp").unwrap();
    layout(temp.path());
    let data = temp.path().join("Data");
    fs::write(data.join("Sample.dds"), b"inert").unwrap();
    let binding = bind_installation(temp.path(), &LAYOUT).unwrap();
    let target =
        resolve_existing_parent(&binding, &RelativePath::parse("sample.dds").unwrap()).unwrap();
    assert_eq!(target.leaf(), "Sample.dds");
    fs::rename(data.join("Sample.dds"), data.join("SAMPLE.dds")).unwrap();
    assert!(
        matches!(observe_target(&target), Ok(TargetObservation::Regular { fingerprint }) if fingerprint.len == 5)
    );
}

#[test]
fn observation_refuses_a_new_ascii_alias() {
    let temp = tempfile::tempdir_in("/var/tmp").unwrap();
    layout(temp.path());
    let data = temp.path().join("Data");
    fs::write(data.join("Sample.dds"), b"inert").unwrap();
    let binding = bind_installation(temp.path(), &LAYOUT).unwrap();
    let target =
        resolve_existing_parent(&binding, &RelativePath::parse("sample.dds").unwrap()).unwrap();
    fs::write(data.join("SAMPLE.dds"), b"alias").unwrap();
    assert_eq!(observe_target(&target), Err(RootError::AmbiguousName));
}

#[test]
fn detached_parent_is_not_an_absent_leaf() {
    let temp = tempfile::tempdir_in("/var/tmp").unwrap();
    layout(temp.path());
    let parent = temp.path().join("Data/Textures");
    fs::create_dir(&parent).unwrap();
    let binding = bind_installation(temp.path(), &LAYOUT).unwrap();
    let target = resolve_existing_parent(
        &binding,
        &RelativePath::parse("Textures/missing.dds").unwrap(),
    )
    .unwrap();
    fs::remove_dir(&parent).unwrap();
    assert_eq!(observe_target(&target), Err(RootError::TargetUnavailable));
}

#[test]
fn tmpfs_identity_supports_binding_and_observation() {
    let temp = tempfile::tempdir_in("/dev/shm").expect("Linux tmpfs witness");
    layout(temp.path());
    fs::write(temp.path().join("Data/Sample.dds"), b"inert").unwrap();
    let binding = bind_installation(temp.path(), &LAYOUT).unwrap();
    let (root, data) = binding.identities();
    assert!(matches!(
        data.evidence(),
        NativeIdentity::Linux {
            fs_magic: 0x0102_1994,
            tree_id: None,
            ..
        }
    ));
    let reopened = reopen_installation(temp.path(), &LAYOUT, root, data).unwrap();
    let target =
        resolve_existing_parent(&reopened, &RelativePath::parse("sample.dds").unwrap()).unwrap();
    assert!(
        matches!(observe_target(&target), Ok(TargetObservation::Regular { fingerprint }) if fingerprint.len == 5)
    );
}

#[cfg(all(target_arch = "x86_64", target_endian = "little"))]
#[test]
fn unavailable_flags_probe_refuses_an_original() {
    let temp = tempfile::tempdir_in("/var/tmp").unwrap();
    layout(temp.path());
    fs::write(temp.path().join("Data/Sample.dds"), b"inert").unwrap();
    let binding = bind_installation(temp.path(), &LAYOUT).unwrap();
    let target =
        resolve_existing_parent(&binding, &RelativePath::parse("sample.dds").unwrap()).unwrap();

    // Filter on this test thread only. seccomp_data.nr is at offset 0 and
    // args[1] (the x86-64 ioctl request) starts at 24.
    let instruction = |code, jt, jf, k| libc::sock_filter { code, jt, jf, k };
    let mut filters = [
        instruction(0x20, 0, 0, 0), // load syscall number
        instruction(0x15, 0, 3, libc::SYS_ioctl as u32),
        instruction(0x20, 0, 0, 24),          // load ioctl request
        instruction(0x15, 0, 1, 0x8008_6601), // FS_IOC_GETFLAGS
        instruction(0x06, 0, 0, libc::SECCOMP_RET_ERRNO | libc::ENOTTY as u32),
        instruction(0x06, 0, 0, libc::SECCOMP_RET_ALLOW),
    ];
    let program = libc::sock_fprog {
        len: filters.len() as u16,
        filter: filters.as_mut_ptr(),
    };
    assert_eq!(
        unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) },
        0
    );
    let result = unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            libc::SECCOMP_SET_MODE_FILTER,
            0,
            &program,
        )
    };
    assert_eq!(result, 0, "{}", std::io::Error::last_os_error());
    assert_eq!(
        observe_target(&target),
        Ok(TargetObservation::Unsupported {
            reason: UnsupportedReason::Metadata
        })
    );
}
