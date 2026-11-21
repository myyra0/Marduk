#[path = "scratch.rs"]
mod scratch;

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use marduk_core::deployment::{BoundTarget, DeploymentInputs, OperationId, PreState};
use marduk_core::ids::InstalledRevisionId;
use marduk_core::recovery::{
    Artifact, BackupSlot, BarrierOutcome, CopyEffects, EffectOutcome, Observation, PreImage,
    PreparedStep, StepDirection, StepOutput, StepSpec, matches_post,
};
use marduk_core::root_identity::{Fingerprint, GameLayout, RootError};
use marduk_core::targets::{RelativePath, RootRole, TargetFile};
use marduk_platform::native_proof::{Checkpoint, Fault, ProofSession};
use marduk_platform::{bind_installation, observe_target, resolve_existing_parent};

use scratch::{Worker, pause, scratch};

const LAYOUT: GameLayout = GameLayout {
    data: "Data",
    markers: &["fixture.marker"],
};
const WINNER: &str = "textures/winner.dds";
const NEW: &str = "textures/new.dds";
const SOURCE: &str = "winner.dds";
const EFFECT_CHILD: &str = "effects::effect_child";
const BASE: &[u8] = b"base\n";
const MOD: &[u8] = b"mod\n";
const EDIT: &[u8] = b"edit\n";

struct Fixture<'a> {
    game: &'a Path,
    library: &'a Path,
}

impl Fixture<'_> {
    fn create(&self) {
        fs::create_dir_all(self.game.join("Data/textures")).unwrap();
        fs::create_dir_all(self.game.join("outside")).unwrap();
        fs::create_dir_all(self.library.join("revisions/1/payload")).unwrap();
        fs::write(self.game.join("fixture.marker"), b"inert").unwrap();
        fs::write(self.game.join("Data/untouched.txt"), b"keep\n").unwrap();
        fs::write(self.game.join("outside/sentinel"), b"outside\n").unwrap();
        fs::write(self.game.join("outside/winner.dds"), b"outside winner\n").unwrap();
        fs::write(self.target(WINNER), BASE).unwrap();
        fs::write(self.source(), MOD).unwrap();
    }

    fn target(&self, name: &str) -> PathBuf {
        self.game.join("Data").join(name)
    }

    fn source(&self) -> PathBuf {
        self.library.join("revisions/1/payload").join(SOURCE)
    }

    fn session(&self, hook: impl FnMut(Checkpoint) -> Fault + 'static) -> ProofSession {
        let binding = bind_installation(self.game, &LAYOUT).unwrap();
        let (installation, data) = binding.identities();
        ProofSession::open(
            self.game,
            &LAYOUT,
            installation,
            data,
            &self.game.join("private"),
            self.library,
            Box::new(hook),
        )
        .unwrap()
    }

    fn check_sentinels(&self) {
        bytes(&self.game.join("Data/untouched.txt"), b"keep\n");
        bytes(&self.game.join("outside/sentinel"), b"outside\n");
        bytes(&self.game.join("outside/winner.dds"), b"outside winner\n");
        let mut outside: Vec<_> = fs::read_dir(self.game.join("outside"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        outside.sort();
        assert_eq!(outside, ["sentinel", "winner.dds"]);
        bytes(&self.source(), MOD);
    }
}

fn bytes(path: &Path, expected: &[u8]) {
    assert_eq!(fs::read(path).unwrap(), expected, "{path:?}");
}

fn absent(path: &Path) {
    assert_eq!(
        fs::symlink_metadata(path).unwrap_err().kind(),
        io::ErrorKind::NotFound,
        "{path:?}"
    );
}

fn logical(name: &str) -> TargetFile {
    TargetFile {
        root: RootRole::Data,
        relative: RelativePath::parse(name).unwrap(),
    }
}

fn fingerprint(port: &mut impl CopyEffects, target: &BoundTarget) -> Fingerprint {
    let Observation::Regular(fp) = port.observe(target) else {
        panic!("expected eligible regular original")
    };
    fp
}

fn prepare(port: &mut ProofSession, name: &str, op: u64, detach: bool) -> PreparedStep {
    let op = OperationId::try_from_u64(op).unwrap();
    let target = port.resolve_target(&logical(name)).unwrap();
    let pre = match port.observe(&target) {
        Observation::Absent => PreState::Absent,
        Observation::Regular(fp) => PreState::Regular(fp),
        other => panic!("unexpected pre-state {other:?}"),
    };
    let output = if detach {
        StepOutput::Absent
    } else {
        let source = port
            .source(
                InstalledRevisionId::new(1),
                &RelativePath::parse(SOURCE).unwrap(),
            )
            .unwrap();
        StepOutput::Source(source)
    };
    let step = port
        .prepare(
            op,
            0,
            StepDirection::Forward,
            &StepSpec {
                target,
                original: PreImage {
                    observation: pre.clone(),
                    backup: None,
                },
                pre,
                output,
            },
        )
        .unwrap();
    assert_eq!(step.retain.slot(), BackupSlot::Displaced);
    assert_eq!(port.inspect_alternate_displacement(&step), Artifact::Absent);
    step
}

fn apply(port: &mut ProofSession, step: &PreparedStep) -> EffectOutcome {
    let target = &step.spec.target;
    match (&step.spec.pre, &step.staged) {
        (PreState::Absent, Some((stage, _))) => port.publish_new_absent(target, *stage),
        (PreState::Regular(expected), Some((stage, _))) => {
            port.replace_existing_retain_displaced(target, expected, *stage, step.retain)
        }
        (PreState::Regular(expected), None) => {
            port.detach_existing_retain(target, expected, step.retain)
        }
        _ => panic!("invalid proof step"),
    }
}

fn settle(port: &mut ProofSession, step: &PreparedStep) {
    let EffectOutcome::Observed(post) = apply(port, step) else {
        panic!("clean effect refused")
    };
    assert!(matches_post(step, &post), "{post:?}");
    assert_eq!(port.inspect_step(step), post);
    assert_eq!(
        port.durability_barrier(step, &post),
        BarrierOutcome::Durable(post)
    );
}

fn stage_path(port: &ProofSession, step: &PreparedStep) -> PathBuf {
    port.stage_path(step.staged.as_ref().unwrap().0)
}

fn race(root: &Path, mode: &'static str) -> impl FnMut(Checkpoint) -> Fault + 'static {
    let root = root.to_owned();
    move |event| {
        eprintln!("R03 {mode}: {event:?}");
        if matches!(event, Checkpoint::BeforeNativeEffect) {
            Worker::spawn(EFFECT_CHILD, mode, &root).finish();
        }
        Fault::None
    }
}

#[test]
#[ignore = "worker: run only through a parent proof case"]
fn effect_child() {
    let root = PathBuf::from(std::env::var_os("MARDUK_R03_CHILD_ROOT").unwrap());
    let mode = std::env::var("MARDUK_R03_CHILD_MODE").unwrap();
    assert!(root.join("fixture.marker").is_file());
    match mode.as_str() {
        "create" => {
            use std::io::Write;
            File::options()
                .write(true)
                .create_new(true)
                .open(root.join("Data").join(NEW))
                .unwrap()
                .write_all(EDIT)
                .unwrap();
            File::create(root.join("raced")).unwrap();
        }
        "swap" => {
            fs::rename(root.join("Data").join(WINNER), root.join("old")).unwrap();
            fs::write(root.join("Data").join(WINNER), EDIT).unwrap();
            File::create(root.join("raced")).unwrap();
        }
        "parent" => {
            let cut = root.parent().unwrap();
            let parent = root.join("Data/textures");
            match fs::rename(&parent, cut.join("moved-parent")) {
                Ok(()) => {
                    redirect(&root.join("outside"), &parent);
                    File::create(cut.join("substituted")).unwrap();
                }
                Err(err) => {
                    eprintln!("R03 parent relocation refusal: {err:?}");
                    assert!(pin_refusal(&err), "{err:?}");
                    File::create(cut.join("pinned")).unwrap();
                }
            }
        }
        "before-publish" | "before-barrier" | "after-barrier" => {
            let library = root.join("library");
            let f = Fixture {
                game: &root,
                library: &library,
            };
            let checkpoint_root = root.clone();
            let before = mode == "before-publish";
            let mut port = f.session(move |event| {
                if before && matches!(event, Checkpoint::BeforeNativeEffect) {
                    pause(&checkpoint_root);
                }
                Fault::None
            });
            let step = prepare(&mut port, NEW, 1, false);
            let EffectOutcome::Observed(post) = apply(&mut port, &step) else {
                panic!("publish refused")
            };
            assert!(matches_post(&step, &post));
            if mode == "after-barrier" {
                let barrier = port.durability_barrier(&step, &post);
                if cfg!(windows)
                    && matches!(
                        barrier,
                        BarrierOutcome::Unavailable(RootError::UnsupportedTarget(
                            marduk_core::root_identity::UnsupportedReason::Configuration
                        ))
                    )
                {
                    File::create(root.join("barrier-blocked")).unwrap();
                } else {
                    assert_eq!(barrier, BarrierOutcome::Durable(post));
                }
            }
            pause(&root);
        }
        #[cfg(windows)]
        "hold" => {
            use std::os::windows::fs::OpenOptionsExt;
            let _file = File::options()
                .read(true)
                .share_mode(1)
                .open(root.join("Data").join(WINNER))
                .unwrap();
            pause(&root);
        }
        _ => panic!("unknown child mode"),
    }
}

#[test]
fn native_no_replace_honors_concurrent_create() {
    let Some((dir, _)) = scratch() else { return };
    let library = dir.path().join("library");
    let f = Fixture {
        game: dir.path(),
        library: &library,
    };
    f.create();
    let mut port = f.session(race(f.game, "create"));
    let step = prepare(&mut port, NEW, 1, false);
    let outcome = apply(&mut port, &step);
    assert!(
        dir.path().join("raced").is_file(),
        "race checkpoint was not reached"
    );
    assert!(matches!(outcome, EffectOutcome::Conflict(_)), "{outcome:?}");
    bytes(&f.target(NEW), EDIT);
    bytes(&stage_path(&port, &step), MOD);
    assert_eq!(
        port.inspect_step(&step).stage,
        Artifact::Regular(step.staged.unwrap().1)
    );
    f.check_sentinels();
}

fn raced_displacement(detach: bool) {
    let Some((dir, _)) = scratch() else { return };
    let library = dir.path().join("library");
    let f = Fixture {
        game: dir.path(),
        library: &library,
    };
    f.create();
    let mut run_race = race(f.game, "swap");
    let edited = std::rc::Rc::new(std::cell::RefCell::new(None));
    let reader = edited.clone();
    let target_path = f.target(WINNER);
    let mut port = f.session(move |event| {
        let before = matches!(&event, Checkpoint::BeforeNativeEffect);
        let result = run_race(event);
        if before {
            *reader.borrow_mut() = Some(witness(&target_path));
        }
        result
    });
    let step = prepare(&mut port, WINNER, 1, detach);
    let PreState::Regular(base) = &step.spec.pre else {
        unreachable!()
    };
    let outcome = apply(&mut port, &step);
    assert!(
        dir.path().join("raced").is_file(),
        "race checkpoint was not reached"
    );
    bytes(&dir.path().join("old"), BASE);
    let original = step.spec.original.backup.unwrap();
    let step_pre = step.step_pre.backup.unwrap();
    assert_ne!(original, step_pre);
    for backup in [original, step_pre] {
        bytes(&port.backup_path(backup), BASE);
        port.validate_backup(backup, base).unwrap();
    }
    let evidence = port.inspect_step(&step);
    let edited = edited.borrow();
    let edited = edited.as_ref().expect("external object witness");
    match outcome {
        EffectOutcome::Conflict(reported) => {
            assert_eq!(reported, evidence);
            assert_eq!(evidence.retained, Artifact::Absent);
            bytes(&f.target(WINNER), EDIT);
            assert_eq!(&witness(&f.target(WINNER)), edited);
            if !detach {
                bytes(&stage_path(&port, &step), MOD);
            }
        }
        EffectOutcome::Displaced {
            evidence: reported,
            at,
        } => {
            assert_eq!(at, step.retain);
            assert_eq!(reported, evidence);
            bytes(&port.backup_path(at), EDIT);
            assert_eq!(&witness(&port.backup_path(at)), edited);
            let Artifact::Regular(actual) = evidence.retained else {
                panic!("lost moved object")
            };
            assert_ne!(actual.identity, base.identity);
            if detach {
                absent(&f.target(WINNER));
            } else {
                bytes(&f.target(WINNER), MOD);
            }
            assert_eq!(evidence.stage, Artifact::Absent);
        }
        other => panic!("unexpected race outcome: {other:?}"),
    }
    assert_eq!(port.inspect_alternate_displacement(&step), Artifact::Absent);
    f.check_sentinels();
}

#[test]
fn native_replace_records_actually_displaced_file() {
    raced_displacement(false);
}

#[cfg(target_os = "linux")]
fn witness(path: &Path) -> (u64, u64, u32, u32, u32, i64, i64) {
    use std::os::unix::fs::MetadataExt;
    let m = fs::symlink_metadata(path).unwrap();
    (
        m.dev(),
        m.ino(),
        m.mode(),
        m.uid(),
        m.gid(),
        m.mtime(),
        m.mtime_nsec(),
    )
}

#[cfg(windows)]
fn witness(path: &Path) -> (u64, [u8; 16], u32, u64) {
    use std::os::windows::{fs::MetadataExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx,
    };

    let file = File::open(path).unwrap();
    let mut info = std::mem::MaybeUninit::<FILE_ID_INFO>::uninit();
    assert_ne!(
        unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileIdInfo,
                info.as_mut_ptr().cast(),
                size_of::<FILE_ID_INFO>() as u32,
            )
        },
        0
    );
    let info = unsafe { info.assume_init() };
    let metadata = file.metadata().unwrap();
    (
        info.VolumeSerialNumber,
        info.FileId.Identifier,
        metadata.file_attributes(),
        metadata.last_write_time(),
    )
}

#[test]
fn native_detach_preserves_unknown_file() {
    raced_displacement(true);
}

#[test]
fn native_rejects_root_and_parent_substitution() {
    let Some((dir, _)) = scratch() else { return };
    let game = dir.path().join("game");
    let library = dir.path().join("library");
    let f = Fixture {
        game: &game,
        library: &library,
    };
    f.create();
    let mut port = f.session(race(&game, "parent"));
    let step = prepare(&mut port, WINNER, 1, false);
    let outcome = apply(&mut port, &step);
    if dir.path().join("substituted").is_file() {
        assert!(
            matches!(
                outcome,
                EffectOutcome::Unavailable(
                    RootError::OutsideBoundary | RootError::TargetUnavailable
                ) | EffectOutcome::Conflict(_)
            ),
            "{outcome:?}"
        );
        bytes(&dir.path().join("moved-parent/winner.dds"), BASE);
        bytes(&stage_path(&port, &step), MOD);
        assert_eq!(port.inspect_alternate_displacement(&step), Artifact::Absent);
        unredirect(&f.target("textures"));
    } else {
        assert!(
            dir.path().join("pinned").is_file(),
            "parent race not attempted"
        );
        assert!(matches!(outcome, EffectOutcome::Observed(_)), "{outcome:?}");
        bytes(&f.target(WINNER), MOD);
    }
    f.check_sentinels();
    let binding = bind_installation(&game, &LAYOUT).unwrap();
    let (installation, data) = binding.identities();
    let (installation, data) = (installation.clone(), data.clone());
    drop(binding);
    drop(port);
    let old = dir.path().join("old-game");
    fs::rename(&game, &old).unwrap();
    f.create();
    let expected = fs::read(f.target(WINNER)).unwrap();
    assert!(matches!(
        ProofSession::open(
            &game,
            &LAYOUT,
            &installation,
            &data,
            &old.join("private"),
            &library,
            Box::new(|_| Fault::None),
        ),
        Err(RootError::TargetUnavailable)
    ));
    bytes(&f.target(WINNER), &expected);
    f.check_sentinels();
}

#[cfg(target_os = "linux")]
fn redirect(to: &Path, from: &Path) {
    std::os::unix::fs::symlink(to, from).unwrap();
}

#[cfg(windows)]
fn redirect(to: &Path, from: &Path) {
    assert!(std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", "New-Item -ErrorAction Stop -ItemType Junction -Path $env:R03_LINK -Target $env:R03_TARGET"])
        .env("R03_LINK", from).env("R03_TARGET", to).status().unwrap().success());
}

#[cfg(target_os = "linux")]
fn unredirect(path: &Path) {
    fs::remove_file(path).unwrap();
}

#[cfg(windows)]
fn unredirect(path: &Path) {
    fs::remove_dir(path).unwrap();
}

fn pin_refusal(error: &io::Error) -> bool {
    cfg!(windows) && matches!(error.raw_os_error(), Some(5 | 32 | 33))
}

#[test]
fn native_error_does_not_prove_no_change() {
    let Some((dir, _)) = scratch() else { return };
    let library = dir.path().join("library");
    let f = Fixture {
        game: dir.path(),
        library: &library,
    };
    f.create();
    let mut port = f.session(|event| {
        eprintln!("R03 effect return: {event:?}");
        if matches!(event, Checkpoint::AfterNativeEffect { raw_error: None }) {
            Fault::LoseAcknowledgement
        } else {
            Fault::None
        }
    });
    let step = prepare(&mut port, WINNER, 1, false);
    let outcome = apply(&mut port, &step);
    let EffectOutcome::Ambiguous(reported) = outcome else {
        panic!("lost ACK became {outcome:?}")
    };
    assert_eq!(port.inspect_step(&step), reported);
    assert!(matches_post(&step, &reported), "{reported:?}");
    bytes(&f.target(WINNER), MOD);
    bytes(&port.backup_path(step.retain), BASE);
    assert_eq!(reported.stage, Artifact::Absent);
    f.check_sentinels();
    drop(port);
    actual_refusals(&f);
}

#[cfg(target_os = "linux")]
fn actual_refusals(f: &Fixture<'_>) {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};

    for metadata in [false, true] {
        let mut port = f.session(|event| {
            eprintln!("R03 refusal: {event:?}");
            Fault::None
        });
        let step = prepare(&mut port, WINNER, if metadata { 3 } else { 2 }, false);
        if metadata {
            let path = CString::new(f.target(WINNER).as_os_str().as_bytes()).unwrap();
            assert_eq!(
                unsafe {
                    libc::setxattr(
                        path.as_ptr(),
                        c"user.r03".as_ptr(),
                        b"y".as_ptr().cast(),
                        1,
                        0,
                    )
                },
                0
            );
        } else {
            fs::rename(f.target(WINNER), f.game.join("missing-original")).unwrap();
        }
        let outcome = apply(&mut port, &step);
        assert!(
            !matches!(outcome, EffectOutcome::Observed(_)),
            "{outcome:?}"
        );
        assert_eq!(port.inspect_step(&step).retained, Artifact::Absent);
        bytes(&stage_path(&port, &step), MOD);
        if metadata {
            bytes(&f.target(WINNER), MOD);
        } else {
            absent(&f.target(WINNER));
            bytes(&f.game.join("missing-original"), MOD);
            fs::rename(f.game.join("missing-original"), f.target(WINNER)).unwrap();
        }
    }
}

#[cfg(windows)]
fn actual_refusals(f: &Fixture<'_>) {
    let mut port = f.session(|event| {
        eprintln!("R03 refusal: {event:?}");
        Fault::None
    });
    let step = prepare(&mut port, WINNER, 2, false);
    let mut child = Worker::spawn(EFFECT_CHILD, "hold", f.game);
    child.checkpoint(&f.game.join("child-ready"));
    let outcome = apply(&mut port, &step);
    child.interrupt();
    assert!(
        !matches!(outcome, EffectOutcome::Observed(_)),
        "{outcome:?}"
    );
    let post = port.inspect_step(&step);
    assert_eq!(post.retained, Artifact::Absent);
    assert_eq!(
        post.stage,
        Artifact::Regular(step.staged.as_ref().unwrap().1.clone())
    );
    bytes(&stage_path(&port, &step), MOD);
    let PreState::Regular(fp) = step.spec.pre else {
        unreachable!()
    };
    assert_eq!(post.target, Observation::Regular(fp));
    bytes(&f.target(WINNER), MOD);
    drop(port);

    let mut port = f.session(|event| {
        eprintln!("R03 missing destination: {event:?}");
        Fault::None
    });
    let step = prepare(&mut port, WINNER, 3, false);
    let moved = f.game.join("missing-original");
    let before = witness(&f.target(WINNER));
    fs::rename(f.target(WINNER), &moved).unwrap();
    let outcome = apply(&mut port, &step);
    assert!(
        !matches!(outcome, EffectOutcome::Observed(_)),
        "{outcome:?}"
    );
    let post = port.inspect_step(&step);
    assert_eq!(post.retained, Artifact::Absent);
    assert_eq!(
        post.stage,
        Artifact::Regular(step.staged.as_ref().unwrap().1.clone())
    );
    absent(&f.target(WINNER));
    bytes(&moved, MOD);
    assert_eq!(witness(&moved), before);
    bytes(&stage_path(&port, &step), MOD);
    fs::rename(moved, f.target(WINNER)).unwrap();
    drop(port);
    windows_metadata_refusals(f);
}

#[cfg(windows)]
fn windows_metadata_refusals(f: &Fixture<'_>) {
    use marduk_core::root_identity::UnsupportedReason;
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows_sys::Wdk::Storage::FileSystem::NtSetEaFile;
    use windows_sys::Win32::{Storage::FileSystem::FILE_WRITE_EA, System::IO::IO_STATUS_BLOCK};

    let mut port = f.session(|_| Fault::None);
    for name in ["ads", "ea", "label"] {
        let relative = format!("textures/{name}.dds");
        let target = f.target(&relative);
        fs::write(&target, BASE).unwrap();
        let before = port.resolve_target(&logical(&relative)).unwrap();
        assert!(matches!(port.observe(&before), Observation::Regular(_)));
        match name {
            "ads" => fs::write(target.with_extension("dds:Zone.Identifier"), b"ZoneId=3").unwrap(),
            "ea" => {
                let file = File::options()
                    .access_mode(FILE_WRITE_EA)
                    .write(true)
                    .open(&target)
                    .unwrap();
                let mut status = std::mem::MaybeUninit::<IO_STATUS_BLOCK>::zeroed();
                // FILE_FULL_EA_INFORMATION: one final entry, two-byte name, one-byte value.
                let mut entry = [0, 0, 0, 0, 0, 2, 1, 0, b'R', b'3', 0, b'x'];
                let code = unsafe {
                    NtSetEaFile(
                        file.as_raw_handle(),
                        status.as_mut_ptr(),
                        entry.as_mut_ptr().cast(),
                        entry.len() as u32,
                    )
                };
                if code != 0 {
                    eprintln!("R03 NOT RUN: host refused EA fixture NTSTATUS={code:#x}");
                    continue;
                }
            }
            "label" => {
                let status = std::process::Command::new("icacls")
                    .arg(&target)
                    .args(["/setintegritylevel", "L"])
                    .status()
                    .unwrap();
                if !status.success() {
                    eprintln!("R03 NOT RUN: host refused explicit label fixture: {status}");
                    continue;
                }
            }
            _ => unreachable!(),
        }
        let error = RootError::UnsupportedTarget(UnsupportedReason::Metadata);
        match port.resolve_target(&logical(&relative)) {
            Ok(bound) => assert_eq!(port.observe(&bound), Observation::Unavailable(error)),
            Err(actual) => assert_eq!(actual, error),
        }
        bytes(&target, BASE);
    }
}

#[test]
fn native_namespace_survives_claimed_barrier() {
    for cut in ["before-publish", "before-barrier", "after-barrier"] {
        let Some((dir, _)) = scratch() else { return };
        let library = dir.path().join("library");
        let f = Fixture {
            game: dir.path(),
            library: &library,
        };
        f.create();
        let target = f.target(NEW);
        absent(&target);
        let mut child = Worker::spawn(EFFECT_CHILD, cut, f.game);
        child.checkpoint(&f.game.join("child-ready"));
        let before = independent_target(&f, NEW);
        child.interrupt();
        let after = independent_target(&f, NEW);
        assert_eq!(before, after);
        if cut == "before-publish" {
            absent(&target);
        } else {
            bytes(&target, MOD);
            assert!(matches!(
                after,
                marduk_core::root_identity::TargetObservation::Regular { .. }
            ));
        }
        f.check_sentinels();
        if dir.path().join("barrier-blocked").is_file() {
            eprintln!("R03 NOT RUN: NTFS namespace barrier unavailable; process visibility only");
        }
        eprintln!("R03 process-only {cut}: {after:?}");
    }
}

fn independent_target(
    f: &Fixture<'_>,
    path: &str,
) -> marduk_core::root_identity::TargetObservation {
    let binding = bind_installation(f.game, &LAYOUT).unwrap();
    let relative = RelativePath::parse(path).unwrap();
    observe_target(&resolve_existing_parent(&binding, &relative).unwrap()).unwrap()
}

#[test]
fn library_catalog_never_names_missing_payload() {
    let Some((dir, _)) = scratch() else { return };
    let Some(probe) = std::env::var_os("MARDUK_R03_LIBRARY_PROBE") else {
        eprintln!("R03 NOT RUN: use run.py to link the real storage probe");
        return;
    };
    let status = std::process::Command::new(probe)
        .arg(dir.path())
        .status()
        .unwrap();
    assert!(status.success(), "{status}");
}

#[cfg(target_os = "linux")]
#[path = "subvolumes.rs"]
mod subvolumes;
