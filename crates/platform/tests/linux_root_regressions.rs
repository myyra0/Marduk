#![cfg(target_os = "linux")]

use std::{fs, os::unix::fs::symlink, path::Path};

use marduk_core::root_identity::{
    GameLayout, NativeIdentity, RootError, RootIdentity, TargetObservation, UnsupportedReason,
};
use marduk_core::targets::RelativePath;
use marduk_platform::{
    WriterGate, bind_installation, observe_target, open_or_create_private_file,
    open_private_directory, reopen_installation, resolve_existing_parent, verify_private_directory,
};

const LAYOUT: GameLayout = GameLayout {
    data: "Data",
    markers: &["fixture.marker"],
};

#[test]
#[ignore = "requires two mount views exposing the same filesystem and objects"]
fn same_objects_on_another_mount_view_keep_identity() {
    let first = std::env::var_os("MARDUK_M1_VIEW_A").expect("first bind mount");
    let second = std::env::var_os("MARDUK_M1_VIEW_B").expect("second bind mount");
    let first = Path::new(&first);
    let second = Path::new(&second);
    assert_ne!(first, second, "the two views must be distinct paths");
    let original = bind_installation(first, &LAYOUT).unwrap();
    let alternate = bind_installation(second, &LAYOUT).unwrap();
    let (root, data) = original.identities();
    let (other_root, other_data) = alternate.identities();
    // Same filesystem objects reached through different mount views: the
    // durable evidence is identical and a different view does not invalidate
    // the recorded installation.
    assert_eq!((other_root, other_data), (root, data));
    let reopened = reopen_installation(second, &LAYOUT, root, data).unwrap();
    assert_eq!(reopened.identities(), (root, data));
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
fn linux_identity_is_exactly_the_durable_object_fields() {
    let temp = tempfile::tempdir_in("/var/tmp").unwrap();
    layout(temp.path());
    let bound = bind_installation(temp.path(), &LAYOUT).unwrap();
    for identity in [bound.identities().0, bound.identities().1] {
        // Exhaustive destructuring: a session field in the token would fail to
        // compile here.
        let NativeIdentity::Linux {
            fs_magic,
            fs_id,
            tree_id,
            ino,
            birth,
        } = *identity.evidence()
        else {
            panic!("expected Linux identity");
        };
        assert_ne!(fs_magic, 0);
        assert_ne!(fs_id, [0; 16]);
        assert_ne!(ino, 0);
        assert_ne!(birth, (0, 0));
        let _ = tree_id;
    }
}

#[test]
fn reopen_requires_exact_object_identity_for_each_root() {
    let temp = tempfile::tempdir_in("/var/tmp").unwrap();
    layout(temp.path());
    let bound = bind_installation(temp.path(), &LAYOUT).unwrap();
    let (root, data) = bound.identities();
    let other_object = |identity: &RootIdentity| {
        let mut native = *identity.evidence();
        let NativeIdentity::Linux { ino, .. } = &mut native else {
            panic!("expected Linux identity");
        };
        *ino = ino.wrapping_add(1);
        let other = RootIdentity::from_adapter_evidence(native);
        assert_ne!(&other, identity);
        other
    };
    assert!(matches!(
        reopen_installation(temp.path(), &LAYOUT, &other_object(root), data),
        Err(RootError::TargetUnavailable)
    ));
    assert!(matches!(
        reopen_installation(temp.path(), &LAYOUT, root, &other_object(data)),
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

#[test]
fn post_open_reverification_refuses_a_substituted_root_and_leaf() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("app-data");
    fs::create_dir(&root).unwrap();
    let pinned = open_private_directory(&root).unwrap();
    let catalog = open_or_create_private_file(&pinned, "catalog.sqlite3").unwrap();
    verify_private_directory(&root, &pinned).unwrap();
    catalog.verify(&pinned).unwrap();

    // SQLite opens the pathname after this point. The path goes through the
    // pinned directory inode under `/proc/self/fd`, so renaming the verified
    // directory cannot redirect it; the root re-proof must still refuse the
    // changed pathname. The reachable substitution is a leaf or sidecar name
    // inside the pinned directory, proven below.
    fs::rename(&root, base.path().join("app-data-old")).unwrap();
    fs::create_dir(&root).unwrap();
    assert_eq!(
        verify_private_directory(&root, &pinned),
        Err(RootError::TargetUnavailable)
    );
    assert!(fs::read_dir(&root).unwrap().next().is_none());

    // A leaf replaced inside the pinned directory is refused by the leaf proof.
    let held = base.path().join("app-data-old").join("catalog.sqlite3");
    fs::rename(&held, base.path().join("app-data-old").join("moved")).unwrap();
    fs::write(&held, b"replacement").unwrap();
    assert_eq!(catalog.verify(&pinned), Err(RootError::TargetUnavailable));
    assert_eq!(fs::read(&held).unwrap(), b"replacement");
}

#[test]
fn substituted_ancestor_cannot_redirect_lock_or_catalog_writes() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("app-data");
    fs::create_dir(&root).unwrap();
    let pinned = open_private_directory(&root).unwrap();

    // Replace the verified directory at its old pathname. A pathname-based
    // open would now land in the attacker-controlled replacement.
    fs::rename(&root, base.path().join("app-data-old")).unwrap();
    fs::create_dir(&root).unwrap();
    let outside = base.path().join("outside.sqlite3");
    fs::write(&outside, b"outside").unwrap();
    symlink(&outside, root.join("catalog.sqlite3")).unwrap();

    let gate = WriterGate::open_at(&pinned, "library.lock").unwrap();
    let _guard = gate.try_lock().unwrap();
    let catalog = open_or_create_private_file(&pinned, "catalog.sqlite3").unwrap();
    let path = catalog.path(&pinned).unwrap();
    assert!(path.starts_with(format!("/proc/self/fd/{}", {
        use std::os::fd::AsRawFd;
        pinned.as_raw_fd()
    })));
    fs::write(&path, b"pinned").unwrap();
    catalog.verify(&pinned).unwrap();

    assert_eq!(
        fs::read(base.path().join("app-data-old").join("catalog.sqlite3")).unwrap(),
        b"pinned"
    );
    assert!(
        base.path()
            .join("app-data-old")
            .join("library.lock")
            .exists()
    );
    assert!(!root.join("library.lock").exists());
    assert_eq!(fs::read(&outside).unwrap(), b"outside");
}
