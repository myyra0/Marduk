//! Review regressions for native proof evidence, displacement and barriers.
//!
//! These are additive Linux witnesses; the frozen R03 harness is untouched.

#![cfg(target_os = "linux")]

#[allow(dead_code)]
#[path = "../../../tests/platform/m1/native-publication/scratch.rs"]
mod scratch;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use marduk_core::deployment::{DeploymentInputs, OperationId, PreState};
use marduk_core::ids::InstalledRevisionId;
use marduk_core::recovery::{
    Artifact, BackupRef, BackupSlot, BarrierOutcome, CopyEffects, EffectOutcome, Observation,
    PreImage, PreparedStep, StepDirection, StepOutput, StepSpec,
};
use marduk_core::root_identity::{Fingerprint, GameLayout};
use marduk_core::targets::{RelativePath, RootRole, TargetFile};
use marduk_platform::bind_installation;
use marduk_platform::native_proof::{Checkpoint, Fault, ProofSession};

use scratch::scratch;

const LAYOUT: GameLayout = GameLayout {
    data: "Data",
    markers: &["fixture.marker"],
};
const WINNER: &str = "textures/winner.dds";
const NEW: &str = "textures/new.dds";
const SOURCE: &str = "winner.dds";
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

fn prepare(port: &mut ProofSession, name: &str) -> PreparedStep {
    let target = port.resolve_target(&logical(name)).unwrap();
    let pre = match port.observe(&target) {
        Observation::Absent => PreState::Absent,
        Observation::Regular(fingerprint) => PreState::Regular(fingerprint),
        other => panic!("unexpected pre-state {other:?}"),
    };
    let source = port
        .source(
            InstalledRevisionId::new(1),
            &RelativePath::parse(SOURCE).unwrap(),
        )
        .unwrap();
    port.prepare(
        OperationId::try_from_u64(1).unwrap(),
        0,
        StepDirection::Forward,
        &StepSpec {
            target,
            original: PreImage {
                observation: pre.clone(),
                backup: None,
            },
            pre,
            output: StepOutput::Source(source),
        },
    )
    .unwrap()
}

fn apply(port: &mut ProofSession, step: &PreparedStep) -> EffectOutcome {
    let target = &step.spec.target;
    let (stage, _) = step.staged.as_ref().expect("staged output");
    match &step.spec.pre {
        PreState::Absent => port.publish_new_absent(target, *stage),
        PreState::Regular(expected) => {
            port.replace_existing_retain_displaced(target, expected, *stage, step.retain)
        }
    }
}

fn stage_path(port: &ProofSession, step: &PreparedStep) -> PathBuf {
    port.stage_path(step.staged.as_ref().unwrap().0)
}

fn base(step: &PreparedStep) -> Fingerprint {
    let PreState::Regular(fingerprint) = &step.spec.pre else {
        panic!("expected a regular pre-image")
    };
    fingerprint.clone()
}

#[test]
fn exchange_evidence_names_a_third_party_displaced_object() {
    let Some((dir, _)) = scratch() else { return };
    let library = dir.path().join("library");
    let f = Fixture {
        game: dir.path(),
        library: &library,
    };
    f.create();
    let target = f.target(WINNER);
    let third_party = f.game.join("outside/third-party.dds");
    let mut replaced = false;
    let mut port = f.session(move |event| {
        if matches!(event, Checkpoint::NativeRename { .. }) && !replaced {
            replaced = true;
            fs::write(&third_party, EDIT).unwrap();
            fs::rename(&third_party, &target).unwrap();
        }
        Fault::None
    });
    let step = prepare(&mut port, WINNER);
    let original = base(&step);

    let EffectOutcome::Displaced { evidence, at } = apply(&mut port, &step) else {
        panic!("expected a displacement report")
    };
    assert_eq!(at, step.retain);
    assert_eq!(evidence, port.inspect_step(&step));
    assert_eq!(evidence.stage, Artifact::Absent);
    let Artifact::Regular(retained) = &evidence.retained else {
        panic!("displaced bytes were not retained: {:?}", evidence.retained)
    };
    assert_ne!(retained.identity, original.identity);
    assert_eq!(retained.len, EDIT.len() as u64);
    bytes(&port.backup_path(at), EDIT);
    bytes(&f.target(WINNER), MOD);
    bytes(&stage_path(&port, &step), EDIT);
}

#[test]
fn alternate_displacement_reads_its_own_slot() {
    let Some((dir, _)) = scratch() else { return };
    let library = dir.path().join("library");
    let f = Fixture {
        game: dir.path(),
        library: &library,
    };
    f.create();
    let mut port = f.session(|_| Fault::None);
    let step = prepare(&mut port, WINNER);
    let stage = stage_path(&port, &step);

    let at = step.alternate_displacement();
    let path = port.backup_path(at);
    fs::write(&path, EDIT).unwrap();

    let Artifact::Regular(seen) = port.inspect_alternate_displacement(&step) else {
        panic!("alternate slot was not observed")
    };
    assert_eq!(seen.len, EDIT.len() as u64);
    bytes(&path, EDIT);

    let inspected = port.inspect_step(&step);
    assert_eq!(inspected.retained, Artifact::Absent);
    assert_eq!(inspected.stage, Artifact::Regular(step.staged.unwrap().1));
    bytes(&stage, MOD);
}

#[test]
fn failed_exchange_without_retained_bytes_is_ambiguous() {
    let Some((dir, _)) = scratch() else { return };
    let library = dir.path().join("library");
    let f = Fixture {
        game: dir.path(),
        library: &library,
    };
    f.create();
    let stage = dir.path().join("private/ops/1/0/forward/stage");
    let observed_stage = stage.clone();
    let mut removed = false;
    let mut port = f.session(move |event| {
        if matches!(event, Checkpoint::NativeRename { .. }) && !removed {
            removed = true;
            fs::remove_file(&stage).unwrap();
        }
        Fault::None
    });
    let step = prepare(&mut port, WINNER);

    let outcome = apply(&mut port, &step);
    let EffectOutcome::Ambiguous(evidence) = outcome else {
        panic!("absent retained bytes became {outcome:?}")
    };
    assert_eq!(evidence.retained, Artifact::Absent);
    assert_eq!(evidence.stage, Artifact::Absent);
    absent(&observed_stage);
    bytes(&f.target(WINNER), BASE);
}

#[test]
fn replace_rejects_wrong_expected_and_wrong_retain_slot() {
    let Some((dir, _)) = scratch() else { return };
    let library = dir.path().join("library");
    let f = Fixture {
        game: dir.path(),
        library: &library,
    };
    f.create();
    let mut port = f.session(|_| Fault::None);
    let step = prepare(&mut port, WINNER);
    let original = base(&step);
    let (staged, _) = step.staged.as_ref().unwrap();

    let mut wrong = original.clone();
    wrong.sha256[0] ^= 0xff;
    let outcome =
        port.replace_existing_retain_displaced(&step.spec.target, &wrong, *staged, step.retain);
    assert!(matches!(outcome, EffectOutcome::Conflict(_)), "{outcome:?}");

    let wrong_retain = BackupRef::from_record(
        step.op,
        step.ordinal,
        StepDirection::Forward,
        BackupSlot::Original,
    );
    let outcome =
        port.replace_existing_retain_displaced(&step.spec.target, &original, *staged, wrong_retain);
    assert!(matches!(outcome, EffectOutcome::Conflict(_)), "{outcome:?}");

    bytes(&f.target(WINNER), BASE);
    bytes(&stage_path(&port, &step), MOD);
}

#[test]
fn barrier_never_claims_durability_without_target_parent_sync() {
    let Some((dir, _)) = scratch() else { return };
    let library = dir.path().join("library");
    let f = Fixture {
        game: dir.path(),
        library: &library,
    };
    f.create();
    let mut port = f.session(|_| Fault::None);
    let step = prepare(&mut port, NEW);
    let EffectOutcome::Observed(post) = apply(&mut port, &step) else {
        panic!("clean publish refused")
    };

    let parent = f.game.join("Data/textures");
    let moved = f.game.join("Data/textures-moved");
    fs::rename(&parent, &moved).unwrap();
    let broken = port.inspect_step(&step);
    assert!(
        matches!(broken.target, Observation::Unavailable(_)),
        "{broken:?}"
    );
    let barrier = port.durability_barrier(&step, &broken);
    assert!(
        matches!(barrier, BarrierOutcome::Unavailable(_)),
        "barrier claimed durability without the parent sync: {barrier:?}"
    );
    assert_ne!(barrier, BarrierOutcome::Durable(post));
    fs::rename(&moved, &parent).unwrap();
}
