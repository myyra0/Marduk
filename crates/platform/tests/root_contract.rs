#![cfg(any(target_os = "linux", windows))]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use marduk_core::root_identity::{
    Fingerprint, GameLayout, NativeIdentity, RootError, RootIdentity, TargetObservation,
    UnsupportedReason,
};
use marduk_core::targets::{PathError, PathViolation, RelativePath};
use marduk_platform::{
    bind_installation, observe_target, reopen_installation, resolve_existing_parent,
};

const LAYOUT: GameLayout = GameLayout {
    data: "Data",
    markers: &["fixture.marker"],
};

fn scratch() -> tempfile::TempDir {
    let prefix = "marduk-r01-private-";
    let mut builder = tempfile::Builder::new();
    builder.prefix(prefix);
    if let Some(parent) = std::env::var_os("MARDUK_M1_GAME_SCRATCH") {
        builder.tempdir_in(parent).unwrap()
    } else if cfg!(target_os = "linux") {
        // /tmp can be tmpfs, which is not a qualified installation filesystem.
        builder.tempdir_in("/var/tmp").unwrap()
    } else {
        builder.tempdir().unwrap()
    }
}

fn layout_at(install: &Path) -> PathBuf {
    let data = install.join("Data");
    fs::create_dir_all(&data).unwrap();
    fs::write(install.join("fixture.marker"), b"inert marker").unwrap();
    data
}

fn relative(path: &str) -> RelativePath {
    RelativePath::parse(path).expect("valid C01 fixture path")
}

fn entries(root: &Path) -> Vec<PathBuf> {
    fn walk(root: &Path, dir: &Path, paths: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            paths.push(path.strip_prefix(root).unwrap().to_owned());
            // Do not follow symlinks or Windows directory junctions.
            if entry.file_type().unwrap().is_dir() {
                walk(root, &path, paths);
            }
        }
    }
    let mut paths = Vec::new();
    walk(root, root, &mut paths);
    paths.sort_unstable();
    paths
}

fn assert_redacted(error: &RootError, scratch: &Path) {
    let diagnostic = format!("{error:?}");
    let absolute = scratch.to_string_lossy();
    let private_component = scratch.file_name().unwrap().to_string_lossy();
    assert!(!diagnostic.contains(absolute.as_ref()), "{diagnostic}");
    assert!(
        !diagnostic.contains(private_component.as_ref()),
        "{diagnostic}"
    );
}

#[test]
fn case_alias_and_reserved_entries_do_not_publish() {
    let scratch = scratch();
    let install = scratch.path().join("game");
    let textures = layout_at(&install).join("Textures");
    fs::create_dir(&textures).unwrap();
    let winner = textures.join("Winner.dds");
    fs::write(&winner, b"original").unwrap();
    let before = entries(scratch.path());

    assert!(matches!(
        RelativePath::parse("textures/NUL.dds"),
        Err(PathError::InvalidComponent {
            index: 1,
            reason: PathViolation::ReservedDeviceName,
        })
    ));
    assert_eq!(entries(scratch.path()), before);

    {
        let binding = bind_installation(&install, &LAYOUT).unwrap();
        let target = resolve_existing_parent(&binding, &relative("textures/winner.dds")).unwrap();
        assert_eq!(target.leaf(), "Winner.dds");
        assert!(matches!(
            observe_target(&target).unwrap(),
            TargetObservation::Regular {
                fingerprint: Fingerprint { len: 8, .. }
            }
        ));
    }
    assert_eq!(entries(scratch.path()), before);
    assert_eq!(fs::read(&winner).unwrap(), b"original");

    #[cfg(target_os = "linux")]
    {
        let alias = textures.join("winner.dds");
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&alias)
            .expect("case-sensitive fixture must create a second name");
        let before = entries(scratch.path());
        let binding = bind_installation(&install, &LAYOUT).unwrap();
        assert!(matches!(
            resolve_existing_parent(&binding, &relative("textures/WINNER.dds")),
            Err(RootError::AmbiguousName)
        ));
        assert_eq!(entries(scratch.path()), before);
        assert_eq!(fs::read(&winner).unwrap(), b"original");
        assert_eq!(fs::read(alias).unwrap(), b"");
    }
}

#[test]
fn verified_data_parent_distinguishes_absence_from_missing_parent() {
    let scratch = scratch();
    let install = scratch.path().join("game");
    let data = layout_at(&install);
    let sentinel = data.join("parent.sentinel");
    let outside = install.join("outside.sentinel");
    fs::write(&sentinel, b"inside Data").unwrap();
    fs::write(&outside, b"outside Data").unwrap();
    let before = entries(scratch.path());

    let binding = bind_installation(&install, &LAYOUT).unwrap();
    let (installation, data) = binding.identities();
    assert_ne!(installation, data);
    let reopened = reopen_installation(&install, &LAYOUT, installation, data).unwrap();
    assert_eq!(reopened.identities(), (installation, data));

    #[cfg(windows)]
    match (installation.evidence(), data.evidence()) {
        (
            NativeIdentity::Windows {
                volume_serial: install_volume,
                file_id: install_id,
                ..
            },
            NativeIdentity::Windows {
                volume_serial: data_volume,
                file_id: data_id,
                ..
            },
        ) => {
            assert_eq!(install_volume, data_volume);
            assert_ne!(install_id, data_id);
        }
        _ => panic!("expected native Windows file identities on NTFS"),
    }

    // This name exists under the installation, but not under its direct Data.
    let target = resolve_existing_parent(&binding, &relative("outside.sentinel")).unwrap();
    let parent: &RootIdentity = target.parent_identity();
    assert_eq!(parent, data);
    assert_eq!(target.leaf(), "outside.sentinel");
    assert!(matches!(
        observe_target(&target),
        Ok(TargetObservation::Absent)
    ));

    let error = resolve_existing_parent(&binding, &relative("Missing/New.dds"))
        .err()
        .expect("missing parent must not resolve");
    assert!(matches!(error, RootError::MissingParent));
    assert_redacted(&error, scratch.path());
    assert_eq!(entries(scratch.path()), before);
    assert_eq!(fs::read(sentinel).unwrap(), b"inside Data");
    assert_eq!(fs::read(outside).unwrap(), b"outside Data");
}

#[test]
fn replaced_root_is_not_a_retry_target() {
    let scratch = scratch();
    let install = scratch.path().join("game");
    let original = scratch.path().join("original");
    let replacement = scratch.path().join("replacement");
    let data = layout_at(&install);
    fs::write(data.join("sentinel.dds"), b"original").unwrap();
    let outside = scratch.path().join("outside.sentinel");
    fs::write(&outside, b"outside").unwrap();
    let binding = bind_installation(&install, &LAYOUT).unwrap();
    let (installation, data) = binding.identities();
    let (installation, data) = (installation.clone(), data.clone());
    // Windows correctly pins directories without delete sharing. Simulate
    // substitution between sessions, not a rename through a live pin.
    drop(binding);

    fs::rename(&install, &original).expect("move original aside");
    fs::write(layout_at(&install).join("sentinel.dds"), b"replacement").unwrap();
    let before = entries(scratch.path());
    let error = reopen_installation(&install, &LAYOUT, &installation, &data)
        .err()
        .expect("same pathname must not authorize replacement");
    assert!(matches!(error, RootError::TargetUnavailable));
    assert_redacted(&error, scratch.path());
    assert_eq!(entries(scratch.path()), before);
    assert_eq!(
        fs::read(install.join("Data/sentinel.dds")).unwrap(),
        b"replacement"
    );
    assert_eq!(
        fs::read(original.join("Data/sentinel.dds")).unwrap(),
        b"original"
    );
    assert_eq!(fs::read(outside).unwrap(), b"outside");

    fs::rename(&install, &replacement).unwrap();
    fs::rename(&original, &install).unwrap();
    let restored = reopen_installation(&install, &LAYOUT, &installation, &data)
        .expect("the original still matches the recorded pair");
    assert_eq!(restored.identities(), (&installation, &data));
}

#[test]
fn symlink_or_junction_component_is_refused() {
    let scratch = scratch();
    let install = scratch.path().join("game");
    let data = layout_at(&install);
    let outside = scratch.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("sentinel.dds"), b"outside").unwrap();

    #[cfg(target_os = "linux")]
    std::os::unix::fs::symlink(&outside, data.join("Redirect")).unwrap();
    #[cfg(windows)]
    run(Command::new("cmd.exe").current_dir(&data).args([
        "/d",
        "/c",
        "mklink",
        "/J",
        "Redirect",
        r"..\..\outside",
    ]));

    assert_eq!(
        fs::read(data.join("Redirect/sentinel.dds")).unwrap(),
        b"outside"
    );
    let before = entries(scratch.path());
    let binding = bind_installation(&install, &LAYOUT).unwrap();
    let error = resolve_existing_parent(&binding, &relative("Redirect/sentinel.dds"))
        .err()
        .expect("redirected parent escaped Data");
    assert!(matches!(error, RootError::OutsideBoundary));
    assert_redacted(&error, scratch.path());
    assert_eq!(entries(scratch.path()), before);
    assert_eq!(fs::read(outside.join("sentinel.dds")).unwrap(), b"outside");
}

#[test]
fn existing_original_with_unsupported_metadata_is_refused() {
    let scratch = scratch();
    let install = scratch.path().join("game");
    let data = layout_at(&install);
    let original = scratch.path().join("outside.sentinel");
    fs::write(&original, b"outside hardlink source").unwrap();
    fs::hard_link(&original, data.join("hardlink.dds")).expect("same-volume hardlink");
    #[cfg(target_os = "linux")]
    run(Command::new("mkfifo").arg(data.join("fifo.dds")));
    let before = entries(scratch.path());

    let binding = bind_installation(&install, &LAYOUT).unwrap();
    let hardlink = resolve_existing_parent(&binding, &relative("hardlink.dds")).unwrap();
    assert!(matches!(
        observe_target(&hardlink),
        Ok(TargetObservation::Unsupported {
            reason: UnsupportedReason::Metadata
        })
    ));
    #[cfg(target_os = "linux")]
    {
        let fifo = resolve_existing_parent(&binding, &relative("fifo.dds")).unwrap();
        assert!(matches!(
            observe_target(&fifo),
            Ok(TargetObservation::Unsupported {
                reason: UnsupportedReason::ObjectKind
            })
        ));
    }
    assert_eq!(entries(scratch.path()), before);
    assert_eq!(
        fs::read(data.join("hardlink.dds")).unwrap(),
        b"outside hardlink source"
    );
    assert_eq!(fs::read(original).unwrap(), b"outside hardlink source");
}

fn run(command: &mut Command) {
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("{command:?}: {error}"));
    assert!(
        output.status.success(),
        "{command:?}: {}\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[cfg(target_os = "linux")]
fn btrfs_scratch() -> tempfile::TempDir {
    let parent = std::env::var_os("MARDUK_M1_BTRFS_SCRATCH")
        .expect("explicit ignored-test invocation requires disposable Btrfs scratch");
    tempfile::tempdir_in(parent).unwrap()
}

#[cfg(target_os = "linux")]
fn btrfs(args: [&str; 2]) -> Command {
    let mut command = Command::new("btrfs");
    command.args(args);
    command
}

#[cfg(target_os = "linux")]
struct Subvolumes(Vec<PathBuf>);

#[cfg(target_os = "linux")]
impl Subvolumes {
    fn cleanup(&self) -> bool {
        let mut ok = true;
        for path in self.0.iter().rev() {
            if !path.exists() {
                continue;
            }
            match btrfs(["subvolume", "delete"]).arg(path).output() {
                Ok(output) if output.status.success() => {}
                failure => {
                    ok = false;
                    eprintln!("deleting test subvolume {path:?}: {failure:?}");
                }
            }
        }
        ok
    }
}

#[cfg(target_os = "linux")]
impl Drop for Subvolumes {
    fn drop(&mut self) {
        self.cleanup();
    }
}

#[cfg(target_os = "linux")]
fn btrfs_tree(root: &RootIdentity) -> u64 {
    match root.evidence() {
        NativeIdentity::Linux {
            fs_magic: 0x9123683e,
            tree_id: Some(tree),
            ..
        } => *tree,
        _ => panic!("missing native Btrfs tree identity"),
    }
}

#[test]
#[cfg(target_os = "linux")]
#[ignore = "requires MARDUK_M1_BTRFS_SCRATCH"]
fn btrfs_reopened_snapshot_is_not_original_root() {
    use std::os::unix::fs::MetadataExt;

    let scratch = btrfs_scratch();
    let install = scratch.path().join("game");
    let original = scratch.path().join("original");
    let snapshot = scratch.path().join("snapshot");
    let subvolumes = Subvolumes(vec![install.clone(), original.clone(), snapshot.clone()]);
    run(btrfs(["subvolume", "create"]).arg(&install));
    let outside = scratch.path().join("library.sentinel");
    fs::write(&outside, b"library outside game subvolume").unwrap();
    fs::write(layout_at(&install).join("sentinel.dds"), b"game").unwrap();
    let binding = bind_installation(&install, &LAYOUT).expect("selected root can be a subvolume");
    let (installation, data) = binding.identities();
    let (installation, data) = (installation.clone(), data.clone());
    let original_tree = btrfs_tree(&installation);
    assert_eq!(btrfs_tree(&data), original_tree);
    assert_eq!(
        reopen_installation(&install, &LAYOUT, &installation, &data)
            .unwrap()
            .identities(),
        (&installation, &data)
    );
    drop(binding);

    run(btrfs(["subvolume", "snapshot"])
        .arg(&install)
        .arg(&snapshot));
    for child in ["", "Data"] {
        assert_eq!(
            fs::metadata(install.join(child)).unwrap().ino(),
            fs::metadata(snapshot.join(child)).unwrap().ino(),
            "snapshot must preserve the inode number for {child:?}"
        );
    }
    fs::rename(&install, &original).unwrap();
    fs::rename(&snapshot, &install).unwrap();
    let before = entries(scratch.path());
    let substitute =
        bind_installation(&install, &LAYOUT).expect("substitute layout itself is valid");
    assert_ne!(btrfs_tree(substitute.identities().0), original_tree);
    drop(substitute);
    assert!(matches!(
        reopen_installation(&install, &LAYOUT, &installation, &data),
        Err(RootError::TargetUnavailable)
    ));
    assert_eq!(entries(scratch.path()), before);
    assert_eq!(
        fs::read(install.join("Data/sentinel.dds")).unwrap(),
        b"game"
    );
    assert_eq!(
        fs::read(original.join("Data/sentinel.dds")).unwrap(),
        b"game"
    );
    assert_eq!(
        fs::read(outside).unwrap(),
        b"library outside game subvolume"
    );
    assert!(subvolumes.cleanup(), "failed to clean up Btrfs scratch");
}

#[test]
#[cfg(target_os = "linux")]
#[ignore = "requires MARDUK_M1_BTRFS_SCRATCH"]
fn unapproved_nested_btrfs_subvolume_is_refused() {
    let scratch = btrfs_scratch();
    let install = scratch.path().join("game");
    let data = layout_at(&install);
    let nested = data.join("Nested");
    let subvolumes = Subvolumes(vec![nested.clone()]);
    run(btrfs(["subvolume", "create"]).arg(&nested));
    let sentinel = nested.join("sentinel.dds");
    fs::write(&sentinel, b"separate tree").unwrap();
    let before = entries(scratch.path());
    let binding = bind_installation(&install, &LAYOUT).unwrap();

    assert!(matches!(
        resolve_existing_parent(&binding, &relative("Nested/sentinel.dds")),
        Err(RootError::OutsideBoundary)
    ));
    assert_eq!(entries(scratch.path()), before);
    assert_eq!(fs::read(sentinel).unwrap(), b"separate tree");
    drop(binding);
    assert!(subvolumes.cleanup(), "failed to clean up Btrfs scratch");
}
