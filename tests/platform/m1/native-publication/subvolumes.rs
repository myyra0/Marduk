use super::*;
use marduk_core::root_identity::NativeIdentity;
use std::ffi::CString;
use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
use std::process::Command;

fn subvolume(action: &str, path: &Path) -> bool {
    let result = Command::new("btrfs")
        .args(["subvolume", action])
        .arg(path)
        .output();
    eprintln!("R03 subvolume {action} {path:?}: {result:?}");
    result.is_ok_and(|out| out.status.success())
}

#[test]
fn btrfs_cross_subvolume_copy_does_not_use_cross_subvolume_rename() {
    let Some((dir, kind)) = scratch() else { return };
    if kind != "btrfs" {
        eprintln!("R03 NOT RUN: subvolume case requires Btrfs");
        return;
    }
    let root = dir.keep();
    let pilot = root.join("teardown-check");
    if !subvolume("create", &pilot) {
        eprintln!("R03 NOT RUN: scratch creation refused");
        fs::remove_dir(root).unwrap();
        return;
    }
    if !subvolume("delete", &pilot) {
        eprintln!("R03 NOT RUN: scratch teardown refused; retained {root:?}");
        return;
    }
    let game = root.join("game");
    let library = root.join("library");
    let substitute = root.join("substitute");
    let old_game = root.join("old-game");
    let mut created = vec![];
    for path in [&game, &library, &substitute] {
        if !subvolume("create", path) {
            eprintln!("R03 NOT RUN: scratch creation refused");
            break;
        }
        created.push(path);
    }
    let verdict = if created.len() == 3 {
        std::panic::catch_unwind(|| prove(&game, &library, &substitute, &old_game))
    } else {
        Ok(())
    };
    let mut removed = true;
    for path in [&game, &library, &substitute, &old_game] {
        if path.exists() {
            removed &= subvolume("delete", path);
        }
    }
    if !removed {
        eprintln!("R03 NOT RUN: proof teardown refused; retained {root:?}");
        if let Err(err) = verdict {
            eprintln!("R03 subvolume proof also failed: {err:?}");
        }
        return;
    }
    fs::remove_dir(root).unwrap();
    if let Err(err) = verdict {
        std::panic::resume_unwind(err);
    }
}

fn prove(game: &Path, library: &Path, substitute: &Path, old_game: &Path) {
    let f = Fixture { game, library };
    f.create();
    let candidate = f.target("textures/cross.dds");
    let src = CString::new(f.source().as_os_str().as_bytes()).unwrap();
    let dst = CString::new(candidate.as_os_str().as_bytes()).unwrap();
    assert_eq!(
        unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                src.as_ptr(),
                libc::AT_FDCWD,
                dst.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        },
        -1
    );
    let error = io::Error::last_os_error();
    eprintln!("R03 direct cross-subvolume rename: {error:?}");
    assert_eq!(error.raw_os_error(), Some(libc::EXDEV));
    absent(&candidate);
    bytes(&f.source(), MOD);

    fs::create_dir(library.join("Data")).unwrap();
    fs::write(library.join("fixture.marker"), b"inert").unwrap();
    let library_binding = bind_installation(library, &LAYOUT).unwrap();
    let (_, library_data) = library_binding.identities();
    let calls = std::rc::Rc::new(std::cell::Cell::new(0));
    let observed_calls = calls.clone();
    let mut port = f.session(move |event| {
        eprintln!("R03 Btrfs native effect: {event:?}");
        match event {
            Checkpoint::NativeRename { from, to } => {
                assert!(
                    matches!(
                        (from.evidence(), to.evidence()),
                        (
                            NativeIdentity::Linux { fs_id: a, view_id: av, tree_id: Some(at), .. },
                            NativeIdentity::Linux { fs_id: b, view_id: bv, tree_id: Some(bt), .. },
                        ) if a == b && av == bv && at == bt
                    ),
                    "port attempted a rename across Btrfs domains"
                );
                observed_calls.set(observed_calls.get() + 1);
            }
            Checkpoint::AfterNativeEffect {
                raw_error: Some(libc::EXDEV),
            } => panic!("port received EXDEV"),
            _ => {}
        }
        Fault::None
    });
    let forward = prepare(&mut port, WINNER, 1, false);
    let PreState::Regular(original) = &forward.spec.pre else {
        unreachable!()
    };
    assert!(matches!(
        (&forward.staged.as_ref().unwrap().1.identity, &original.identity, library_data.evidence()),
        (
            NativeIdentity::Linux { fs_id: stage_fs, tree_id: Some(stage), .. },
            NativeIdentity::Linux { fs_id: target_fs, tree_id: Some(target), .. },
            NativeIdentity::Linux { fs_id: library_fs, tree_id: Some(library), .. },
        ) if stage_fs == target_fs && target_fs == library_fs && stage == target && target != library
    ));
    let base = witness(&f.target(WINNER));
    let before = calls.get();
    settle(&mut port, &forward);
    assert!(calls.get() > before, "no native rename trace for replace");
    bytes(&f.target(WINNER), MOD);
    bytes(&port.backup_path(forward.retain), BASE);
    assert_eq!(witness(&port.backup_path(forward.retain)), base);
    let deployed = fingerprint(&mut port, &forward.spec.target);
    let restore = port
        .prepare(
            OperationId::try_from_u64(2).unwrap(),
            0,
            StepDirection::Forward,
            &StepSpec {
                target: forward.spec.target.clone(),
                original: forward.spec.original.clone(),
                pre: PreState::Regular(deployed),
                output: StepOutput::Original(forward.spec.original.backup.unwrap()),
            },
        )
        .unwrap();
    let before = calls.get();
    settle(&mut port, &restore);
    assert!(calls.get() > before, "no native rename trace for Restore");
    bytes(&f.target(WINNER), BASE);
    let restored = fingerprint(&mut port, &forward.spec.target);
    assert_eq!(restored.identity, restore.staged.unwrap().1.identity);
    assert_eq!(restored.metadata, original.metadata);
    assert_eq!(restored.sha256, original.sha256);
    bytes(&port.backup_path(forward.retain), BASE);
    assert_eq!(witness(&port.backup_path(forward.retain)), base);
    f.check_sentinels();
    drop(port);

    let binding = bind_installation(game, &LAYOUT).unwrap();
    let (install, data) = binding.identities();
    let (install, data) = (install.clone(), data.clone());
    drop(binding);
    fs::create_dir_all(substitute.join("Data/textures")).unwrap();
    fs::write(substitute.join("fixture.marker"), b"inert").unwrap();
    fs::write(substitute.join("Data/textures/winner.dds"), EDIT).unwrap();
    assert_eq!(
        fs::metadata(game).unwrap().ino(),
        fs::metadata(substitute).unwrap().ino()
    );
    fs::rename(game, old_game).unwrap();
    fs::rename(substitute, game).unwrap();
    let actual = marduk_platform::reopen_installation(game, &LAYOUT, &install, &data);
    assert!(
        matches!(actual, Err(RootError::TargetUnavailable)),
        "{actual:?}"
    );
    bytes(&f.target(WINNER), EDIT);
    bytes(&old_game.join("Data/textures/winner.dds"), BASE);
}
