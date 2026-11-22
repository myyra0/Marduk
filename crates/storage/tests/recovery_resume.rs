// Recovery witnesses against the real journal with a synthetic effect port.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::rc::Rc;

use marduk_core::deployment::*;
use marduk_core::ids::{InstalledRevisionId, ProfileId, ProfileRevision};
use marduk_core::recovery::*;
use marduk_core::root_identity::{
    FileMetadata, Fingerprint, NativeIdentity, RootError, RootIdentity,
};
use marduk_core::targets::{RelativePath, RootRole, TargetFile};
use marduk_storage::operation_journal::OperationJournal;
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const FILE: &str = "textures/a-only.dds";
const BASE: &[u8] = b"base\n";
const POST: &[u8] = b"b\n";
const EDIT: &[u8] = b"edit\n";

fn native(id: u8) -> NativeIdentity {
    NativeIdentity::Windows {
        volume_serial: 71,
        file_id: [id; 16],
        creation_100ns: 201,
    }
}

fn root(id: u8) -> RootIdentity {
    RootIdentity::from_adapter_evidence(native(id))
}

fn fingerprint(bytes: &[u8], ino: u8) -> Fingerprint {
    Fingerprint {
        identity: native(ino),
        len: bytes.len() as u64,
        sha256: Sha256::digest(bytes).into(),
        metadata: FileMetadata::Windows {
            attributes: 0x20,
            last_write_100ns: 301,
            links: 1,
            security_sha256: [71; 32],
        },
    }
}

fn bound() -> BoundTarget {
    BoundTarget {
        target: TargetFile {
            root: RootRole::Data,
            relative: RelativePath::parse(FILE).unwrap(),
        },
        parent: root(3),
        leaf: "a-only.dds".into(),
    }
}

fn source_ref() -> SourceRef {
    SourceRef {
        installed: InstalledRevisionId::new(1),
        path: RelativePath::parse(FILE).unwrap(),
        sha256: Sha256::digest(POST).into(),
        len: POST.len() as u64,
    }
}

fn fixture_v1() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let db = Connection::open(dir.path().join("catalog.sqlite3")).unwrap();
    db.execute_batch(include_str!("../migrations/0001_library.sql"))
        .unwrap();
    db.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA user_version = 1;
         INSERT INTO profiles (id, revision) VALUES (1, 7);
         INSERT INTO imports
             (id, profile_id, expected_revision, target, display_name, state, owner, archive_sha, result_revision)
             VALUES (1, 1, 6, 'append', 'fixture', 'committed', x'01', zeroblob(32), 7);
         INSERT INTO installed_revisions (id) VALUES (1);
         INSERT INTO members (id, profile_id, ordinal, installed_id, enabled, display_name)
             VALUES (1, 1, 0, 1, 1, 'fixture');",
    )
    .unwrap();
    db.execute(
        "INSERT INTO revision_files (revision_id, ordinal, role, path, len, sha)
         VALUES (1, 0, 'data', ?1, ?2, ?3)",
        rusqlite::params![FILE, POST.len() as i64, Sha256::digest(POST).as_slice()],
    )
    .unwrap();
    dir
}

fn open_fixture(dir: &TempDir) -> OperationJournal {
    let mut journal = OperationJournal::open(dir.path()).unwrap();
    journal
        .select_verified_installation(Path::new("synthetic"), &root(1), &root(2))
        .unwrap();
    journal
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MemFile {
    bytes: Vec<u8>,
    fingerprint: Fingerprint,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    WrongRetain,
    WrongAt,
    WrongTarget,
}

struct MemEffects {
    public: BTreeMap<String, MemFile>,
    staged: BTreeMap<StageRef, MemFile>,
    backups: BTreeMap<BackupRef, MemFile>,
    receipts: BTreeMap<BackupRef, MemFile>,
    prepared: BTreeMap<(OperationId, u32, StepDirection), PreparedStep>,
    sources: BTreeMap<(InstalledRevisionId, String), Vec<u8>>,
    next_ino: u8,
    revision: ProfileRevision,
    fault: Option<Fault>,
    mutations: usize,
    root_gate: Rc<Cell<bool>>,
    unreadable_retains: BTreeSet<BackupRef>,
    unreadable_target: bool,
}

impl MemEffects {
    fn new() -> Self {
        Self {
            public: BTreeMap::from([(
                FILE.to_owned(),
                MemFile {
                    bytes: BASE.to_vec(),
                    fingerprint: fingerprint(BASE, 10),
                },
            )]),
            staged: BTreeMap::new(),
            backups: BTreeMap::new(),
            receipts: BTreeMap::new(),
            prepared: BTreeMap::new(),
            sources: BTreeMap::from([(
                (InstalledRevisionId::new(1), FILE.to_owned()),
                POST.to_vec(),
            )]),
            next_ino: 100,
            revision: ProfileRevision::new(7),
            fault: None,
            mutations: 0,
            root_gate: Rc::default(),
            unreadable_retains: BTreeSet::new(),
            unreadable_target: false,
        }
    }

    fn copy(&mut self, file: MemFile) -> MemFile {
        let fingerprint = fingerprint(&file.bytes, self.next_ino);
        self.next_ino = self.next_ino.wrapping_add(1);
        MemFile {
            bytes: file.bytes,
            fingerprint,
        }
    }

    fn remember(&mut self, step: PreparedStep) {
        self.prepared
            .insert((step.op, step.ordinal, step.direction), step);
    }

    fn store_backup(&mut self, at: BackupRef, file: MemFile) {
        self.receipts.insert(at, file.clone());
        self.backups.insert(at, file);
    }

    fn observe_file(&self, path: &str) -> Observation {
        self.public.get(path).map_or(Observation::Absent, |file| {
            Observation::Regular(file.fingerprint.clone())
        })
    }

    fn artifact(&self, file: Option<&MemFile>) -> Artifact {
        file.map_or(Artifact::Absent, |file| {
            Artifact::Regular(file.fingerprint.clone())
        })
    }

    fn inspect(&mut self, step: &PreparedStep) -> Evidence {
        self.remember(step.clone());
        if self.unreadable_target {
            return Evidence {
                target: Observation::Unavailable(RootError::TargetUnavailable),
                stage: Artifact::Unavailable,
                retained: Artifact::Unavailable,
            };
        }
        Evidence {
            target: self.observe_file(step.spec.target.target.relative.as_str()),
            stage: self.artifact(self.staged.get(
                &step.staged.as_ref().map(|(at, _)| *at).unwrap_or_else(|| {
                    StageRef::from_record(step.op, step.ordinal, step.direction)
                }),
            )),
            retained: if self.unreadable_retains.contains(&step.retain) {
                Artifact::Unavailable
            } else {
                self.artifact(self.backups.get(&step.retain))
            },
        }
    }

    fn mutate(
        &mut self,
        target: &BoundTarget,
        expected: Option<&Fingerprint>,
        stage: Option<StageRef>,
        retain: Option<BackupRef>,
        step: PreparedStep,
    ) -> EffectOutcome {
        self.mutations += 1;
        let path = target.target.relative.as_str();
        let actual = self.public.get(path).map(|file| &file.fingerprint);
        if actual != expected {
            return EffectOutcome::Conflict(self.inspect(&step));
        }
        if let Some(at) = retain {
            let file = self
                .public
                .remove(path)
                .expect("retain requires an existing file");
            self.backups.insert(at, file);
        }
        if let Some(at) = stage {
            let file = self.staged.remove(&at).expect("missing staged output");
            self.public.insert(path.to_owned(), file);
        }
        let mut evidence = self.inspect(&step);
        match self.fault.take() {
            Some(Fault::WrongRetain) => evidence.retained = Artifact::Absent,
            Some(Fault::WrongAt) => {
                let at = BackupRef::from_record(
                    step.op,
                    step.ordinal,
                    StepDirection::Forward,
                    BackupSlot::Original,
                );
                return EffectOutcome::Displaced { evidence, at };
            }
            Some(Fault::WrongTarget) => evidence.target = Observation::Absent,
            None => {}
        }
        match retain {
            Some(at) => EffectOutcome::Displaced { evidence, at },
            None => EffectOutcome::Observed(evidence),
        }
    }
}

struct Gate(Rc<Cell<bool>>);

impl Drop for Gate {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

impl DeploymentInputs for MemEffects {
    type RootGuard = Gate;
    type LibraryGuard = ();

    fn try_root_gate(&mut self) -> Result<Gate, DeployError> {
        if self.root_gate.replace(true) {
            Err(DeployError::Busy)
        } else {
            Ok(Gate(self.root_gate.clone()))
        }
    }

    fn try_library_gate(&mut self) -> Result<(), DeployError> {
        Ok(())
    }

    fn verify_root(
        &mut self,
        installation: &RootIdentity,
        data: &RootIdentity,
    ) -> Result<(), DeployError> {
        if *installation == root(1) && *data == root(2) {
            Ok(())
        } else {
            Err(DeployError::TargetUnavailable)
        }
    }

    fn resolve_target(&mut self, _: &TargetFile) -> Result<BoundTarget, RootError> {
        Ok(bound())
    }

    fn profile_revision(&mut self, _: ProfileId) -> Result<ProfileRevision, DeployError> {
        Ok(self.revision)
    }

    fn source(
        &mut self,
        installed: InstalledRevisionId,
        path: &RelativePath,
    ) -> Result<SourceRef, DeployError> {
        let bytes = self
            .sources
            .get(&(installed, path.as_str().to_owned()))
            .ok_or(DeployError::StalePlan)?;
        Ok(SourceRef {
            installed,
            path: path.clone(),
            sha256: Sha256::digest(bytes).into(),
            len: bytes.len() as u64,
        })
    }
}

impl CopyEffects for MemEffects {
    fn observe(&mut self, target: &BoundTarget) -> Observation {
        self.observe_file(target.target.relative.as_str())
    }

    fn prepare(
        &mut self,
        op: OperationId,
        ordinal: u32,
        direction: StepDirection,
        intent: &StepSpec,
    ) -> Result<PreparedStep, DeployError> {
        let path = intent.target.target.relative.as_str();
        let current = match (&intent.pre, self.public.get(path)) {
            (PreState::Absent, None) => None,
            (PreState::Regular(expected), Some(file)) if *expected == file.fingerprint => {
                Some(file.clone())
            }
            _ => return Err(DeployError::RecoveryConflict),
        };
        let mut spec = intent.clone();
        if matches!(intent.output, StepOutput::Source(_)) {
            spec.original.backup = current.as_ref().map(|file| {
                let at = BackupRef::from_record(op, ordinal, direction, BackupSlot::Original);
                let copy = self.copy(file.clone());
                self.store_backup(at, copy);
                at
            });
        }
        let step_pre = PreImage {
            observation: intent.pre.clone(),
            backup: current.map(|file| {
                let at = BackupRef::from_record(op, ordinal, direction, BackupSlot::Step);
                let copy = self.copy(file);
                self.store_backup(at, copy);
                at
            }),
        };
        let output = match &spec.output {
            StepOutput::Absent => None,
            StepOutput::Source(source) => Some(MemFile {
                bytes: self
                    .sources
                    .get(&(source.installed, source.path.as_str().to_owned()))
                    .ok_or(DeployError::StalePlan)?
                    .clone(),
                fingerprint: fingerprint(POST, 0),
            }),
            StepOutput::Original(at) => Some(
                self.receipts
                    .get(at)
                    .ok_or(DeployError::RecoveryConflict)?
                    .clone(),
            ),
        };
        let staged = output.map(|file| {
            let at = StageRef::from_record(op, ordinal, direction);
            let file = self.copy(file);
            self.staged.insert(at, file.clone());
            (at, file.fingerprint)
        });
        let step = PreparedStep {
            op,
            ordinal,
            direction,
            spec,
            step_pre,
            staged,
            retain: BackupRef::from_record(op, ordinal, direction, BackupSlot::Displaced),
        };
        self.remember(step.clone());
        Ok(step)
    }

    fn publish_new_absent(&mut self, target: &BoundTarget, staged: StageRef) -> EffectOutcome {
        let step = self
            .prepared
            .values()
            .find(|step| step.staged.as_ref().is_some_and(|(at, _)| *at == staged))
            .cloned()
            .expect("prepared create");
        self.mutate(target, None, Some(staged), None, step)
    }

    fn replace_existing_retain_displaced(
        &mut self,
        target: &BoundTarget,
        expected: &Fingerprint,
        staged: StageRef,
        retain: BackupRef,
    ) -> EffectOutcome {
        let step = self
            .prepared
            .values()
            .find(|step| step.staged.as_ref().is_some_and(|(at, _)| *at == staged))
            .cloned()
            .expect("prepared replace");
        self.mutate(target, Some(expected), Some(staged), Some(retain), step)
    }

    fn detach_existing_retain(
        &mut self,
        target: &BoundTarget,
        expected: &Fingerprint,
        retain: BackupRef,
    ) -> EffectOutcome {
        let step = self
            .prepared
            .values()
            .find(|step| step.retain == retain)
            .cloned()
            .expect("prepared detach");
        self.mutate(target, Some(expected), None, Some(retain), step)
    }

    fn inspect_step(&mut self, step: &PreparedStep) -> Evidence {
        self.inspect(step)
    }

    fn inspect_alternate_displacement(&mut self, step: &PreparedStep) -> Artifact {
        let at = step.alternate_displacement();
        if self.unreadable_retains.contains(&at) {
            Artifact::Unavailable
        } else {
            self.artifact(self.backups.get(&at))
        }
    }

    fn durability_barrier(&mut self, step: &PreparedStep, _: &Evidence) -> BarrierOutcome {
        BarrierOutcome::Durable(self.inspect(step))
    }

    fn validate_backup(
        &mut self,
        at: BackupRef,
        expected: &Fingerprint,
    ) -> Result<(), DeployError> {
        let file = self.backups.get(&at).ok_or(DeployError::RecoveryConflict)?;
        let receipt = self
            .receipts
            .get(&at)
            .ok_or(DeployError::RecoveryConflict)?;
        if file == receipt
            && (
                file.fingerprint.sha256,
                file.fingerprint.len,
                file.fingerprint.metadata,
            ) == (expected.sha256, expected.len, expected.metadata)
        {
            Ok(())
        } else {
            Err(DeployError::RecoveryConflict)
        }
    }
}

fn intent() -> FileIntent {
    FileIntent {
        target: bound(),
        source: source_ref(),
        original: PreState::Regular(fingerprint(BASE, 10)),
    }
}

fn save_plan(journal: &mut OperationJournal) -> DeploymentPlan {
    journal
        .save_plan(
            ProfileId::new(1),
            ProfileRevision::new(7),
            root(1),
            root(2),
            vec![intent()],
        )
        .unwrap()
}

fn deploy_step(op: OperationId) -> PreparedStep {
    PreparedStep {
        op,
        ordinal: 0,
        direction: StepDirection::Forward,
        spec: StepSpec {
            target: bound(),
            original: PreImage {
                observation: PreState::Regular(fingerprint(BASE, 10)),
                backup: Some(BackupRef::from_record(
                    op,
                    0,
                    StepDirection::Forward,
                    BackupSlot::Original,
                )),
            },
            pre: PreState::Regular(fingerprint(BASE, 10)),
            output: StepOutput::Source(source_ref()),
        },
        step_pre: PreImage {
            observation: PreState::Regular(fingerprint(BASE, 10)),
            backup: Some(BackupRef::from_record(
                op,
                0,
                StepDirection::Forward,
                BackupSlot::Step,
            )),
        },
        staged: Some((
            StageRef::from_record(op, 0, StepDirection::Forward),
            fingerprint(POST, 0),
        )),
        retain: BackupRef::from_record(op, 0, StepDirection::Forward, BackupSlot::Displaced),
    }
}

fn applied() -> Evidence {
    Evidence {
        target: Observation::Regular(fingerprint(POST, 0)),
        stage: Artifact::Absent,
        retained: Artifact::Regular(fingerprint(BASE, 10)),
    }
}

fn seed_deploy(
    journal: &mut OperationJournal,
    effects: &mut MemEffects,
) -> (OperationId, GenerationId, BackupRef) {
    let plan = save_plan(journal);
    let op = journal.claim(Claim::Deploy(plan)).unwrap();
    let step = deploy_step(op);
    journal.seal(op, vec![step.clone()]).unwrap();
    effects.remember(step.clone());
    effects.staged.insert(
        StageRef::from_record(op, 0, StepDirection::Forward),
        MemFile {
            bytes: POST.to_vec(),
            fingerprint: fingerprint(POST, 0),
        },
    );
    let original_backup =
        BackupRef::from_record(op, 0, StepDirection::Forward, BackupSlot::Original);
    effects.store_backup(
        original_backup,
        MemFile {
            bytes: BASE.to_vec(),
            fingerprint: fingerprint(BASE, 11),
        },
    );
    effects.public.insert(
        FILE.to_owned(),
        MemFile {
            bytes: POST.to_vec(),
            fingerprint: fingerprint(POST, 0),
        },
    );
    journal
        .set_phase(op, Phase::Prepared, Phase::Applying)
        .unwrap();
    journal
        .record_step(op, 0, StepPhase::Ready, StepPhase::Attempting, None)
        .unwrap();
    journal
        .record_step(
            op,
            0,
            StepPhase::Attempting,
            StepPhase::Applied,
            Some(applied()),
        )
        .unwrap();
    let generation = journal
        .finish(op, Finalize::DeployCommitted)
        .unwrap()
        .generation
        .unwrap();
    (op, generation, original_backup)
}

#[test]
fn recovery_reuses_recorded_undo_after_lost_phase_write() {
    let dir = fixture_v1();
    let mut journal = open_fixture(&dir);
    let mut effects = MemEffects::new();
    let plan = save_plan(&mut journal);
    let op = journal.claim(Claim::Deploy(plan)).unwrap();
    let forward = deploy_step(op);
    journal.seal(op, vec![forward.clone()]).unwrap();
    effects.remember(forward.clone());
    effects.public.insert(
        FILE.to_owned(),
        MemFile {
            bytes: POST.to_vec(),
            fingerprint: fingerprint(POST, 0),
        },
    );
    effects.backups.insert(
        forward.retain,
        MemFile {
            bytes: BASE.to_vec(),
            fingerprint: fingerprint(BASE, 10),
        },
    );
    let step_ref = forward.step_pre.backup.unwrap();
    journal
        .set_phase(op, Phase::Prepared, Phase::Applying)
        .unwrap();
    journal
        .record_step(op, 0, StepPhase::Ready, StepPhase::Attempting, None)
        .unwrap();
    journal
        .record_step(
            op,
            0,
            StepPhase::Attempting,
            StepPhase::Applied,
            Some(applied()),
        )
        .unwrap();

    let undo = PreparedStep {
        op,
        ordinal: 0,
        direction: StepDirection::Undo,
        spec: StepSpec {
            target: bound(),
            original: forward.spec.original.clone(),
            pre: PreState::Regular(fingerprint(POST, 0)),
            output: StepOutput::Original(step_ref),
        },
        step_pre: PreImage {
            observation: PreState::Regular(fingerprint(POST, 0)),
            backup: Some(BackupRef::from_record(
                op,
                0,
                StepDirection::Undo,
                BackupSlot::Step,
            )),
        },
        staged: Some((
            StageRef::from_record(op, 0, StepDirection::Undo),
            fingerprint(BASE, 0),
        )),
        retain: BackupRef::from_record(op, 0, StepDirection::Undo, BackupSlot::Displaced),
    };
    journal
        .set_phase(op, Phase::Applying, Phase::RollingBack)
        .unwrap();
    journal.record_undo(op, 0, undo.clone()).unwrap();
    effects.remember(undo.clone());
    effects.staged.insert(
        StageRef::from_record(op, 0, StepDirection::Undo),
        MemFile {
            bytes: BASE.to_vec(),
            fingerprint: fingerprint(BASE, 0),
        },
    );
    effects.store_backup(
        step_ref,
        MemFile {
            bytes: BASE.to_vec(),
            fingerprint: fingerprint(BASE, 11),
        },
    );
    effects.store_backup(
        forward.spec.original.backup.unwrap(),
        MemFile {
            bytes: BASE.to_vec(),
            fingerprint: fingerprint(BASE, 12),
        },
    );
    effects.store_backup(
        undo.step_pre.backup.unwrap(),
        MemFile {
            bytes: POST.to_vec(),
            fingerprint: fingerprint(POST, 13),
        },
    );

    let status = recover_pending(&mut journal, &mut effects, &root(1), &root(2))
        .unwrap()
        .unwrap();
    assert_eq!(status.phase, Phase::RolledBack);
    assert_eq!(effects.public[FILE].bytes, BASE);
    assert_eq!(effects.mutations, 1);
    let rows = journal.steps(op).unwrap();
    assert_eq!(rows[0].phase, StepPhase::Undone);
    assert_eq!(rows[0].undo.as_ref(), Some(&undo));
    assert_eq!(journal.root_state(&root(2)).unwrap().pending, None);
}

#[test]
fn recovery_reexecutes_a_reverted_applied_restore_step() {
    let dir = fixture_v1();
    let mut journal = open_fixture(&dir);
    let mut effects = MemEffects::new();
    let (_deploy, generation, original) = seed_deploy(&mut journal, &mut effects);

    let restore = journal
        .claim(Claim::Restore {
            installation: root(1),
            data: root(2),
            generation,
        })
        .unwrap();
    let step = PreparedStep {
        op: restore,
        ordinal: 0,
        direction: StepDirection::Forward,
        spec: StepSpec {
            target: bound(),
            original: PreImage {
                observation: PreState::Regular(fingerprint(BASE, 10)),
                backup: Some(original),
            },
            pre: PreState::Regular(fingerprint(POST, 0)),
            output: StepOutput::Original(original),
        },
        step_pre: PreImage {
            observation: PreState::Regular(fingerprint(POST, 0)),
            backup: Some(BackupRef::from_record(
                restore,
                0,
                StepDirection::Forward,
                BackupSlot::Step,
            )),
        },
        staged: Some((
            StageRef::from_record(restore, 0, StepDirection::Forward),
            fingerprint(BASE, 0),
        )),
        retain: BackupRef::from_record(restore, 0, StepDirection::Forward, BackupSlot::Displaced),
    };
    journal.seal(restore, vec![step.clone()]).unwrap();
    effects.remember(step.clone());
    effects.store_backup(
        step.step_pre.backup.unwrap(),
        MemFile {
            bytes: POST.to_vec(),
            fingerprint: fingerprint(POST, 13),
        },
    );
    effects.staged.insert(
        StageRef::from_record(restore, 0, StepDirection::Forward),
        MemFile {
            bytes: BASE.to_vec(),
            fingerprint: fingerprint(BASE, 0),
        },
    );
    journal
        .set_phase(restore, Phase::Prepared, Phase::Restoring)
        .unwrap();
    journal
        .record_step(restore, 0, StepPhase::Ready, StepPhase::Attempting, None)
        .unwrap();
    journal
        .record_step(
            restore,
            0,
            StepPhase::Attempting,
            StepPhase::Applied,
            Some(Evidence {
                target: Observation::Regular(fingerprint(BASE, 0)),
                stage: Artifact::Absent,
                retained: Artifact::Regular(fingerprint(POST, 0)),
            }),
        )
        .unwrap();
    // The recorded restore result is no longer visible: revert to the managed file.
    effects.public.insert(
        FILE.to_owned(),
        MemFile {
            bytes: POST.to_vec(),
            fingerprint: fingerprint(POST, 0),
        },
    );

    let status = recover_pending(&mut journal, &mut effects, &root(1), &root(2))
        .unwrap()
        .unwrap();
    assert_eq!(status.phase, Phase::Completed);
    assert_eq!(effects.public[FILE].bytes, BASE);
    assert_eq!(effects.mutations, 1);
    assert_eq!(journal.root_state(&root(2)).unwrap().active, None);
}

#[test]
fn displaced_evidence_is_validated_before_applied() {
    for fault in [Fault::WrongRetain, Fault::WrongAt, Fault::WrongTarget] {
        let dir = fixture_v1();
        let mut journal = open_fixture(&dir);
        let mut effects = MemEffects::new();
        effects.fault = Some(fault);
        let plan = save_plan(&mut journal);
        let result = commit_deployment(&mut journal, &mut effects, plan.id, || false);
        assert!(
            matches!(result, Err(DeployError::RecoveryConflict)),
            "fault {fault:?} produced {result:?}"
        );
        let op = journal.root_state(&root(2)).unwrap().pending.unwrap();
        let status = journal.status(op).unwrap().unwrap();
        assert_eq!(status.phase, Phase::Applying);
        assert_eq!(status.blocker, Some(DeployError::RecoveryConflict));
        let rows = journal.steps(op).unwrap();
        assert_eq!(rows[0].phase, StepPhase::Attempting);
    }
}

#[test]
fn recovery_conflict_returns_status_not_a_generic_error() {
    let dir = fixture_v1();
    let mut journal = open_fixture(&dir);
    let mut effects = MemEffects::new();
    let plan = save_plan(&mut journal);
    let op = journal.claim(Claim::Deploy(plan)).unwrap();
    let step = deploy_step(op);
    journal.seal(op, vec![step]).unwrap();
    journal
        .set_phase(op, Phase::Prepared, Phase::Applying)
        .unwrap();
    journal
        .record_step(op, 0, StepPhase::Ready, StepPhase::Attempting, None)
        .unwrap();
    effects.public.insert(
        FILE.to_owned(),
        MemFile {
            bytes: EDIT.to_vec(),
            fingerprint: fingerprint(EDIT, 90),
        },
    );

    let status = recover_pending(&mut journal, &mut effects, &root(1), &root(2))
        .unwrap()
        .unwrap();
    assert_eq!(status.phase, Phase::RollingBack);
    assert_eq!(status.blocker, Some(DeployError::RecoveryConflict));
    assert_eq!(effects.mutations, 0);
    assert_eq!(effects.public[FILE].bytes, EDIT);
    assert_eq!(journal.root_state(&root(2)).unwrap().pending, Some(op));
}
#[test]
fn interrupted_zero_step_deploy_rolls_back_and_releases_the_lease() {
    let dir = fixture_v1();
    let mut journal = open_fixture(&dir);
    let mut effects = MemEffects::new();
    let empty = journal
        .save_plan(
            ProfileId::new(1),
            ProfileRevision::new(7),
            root(1),
            root(2),
            Vec::new(),
        )
        .unwrap();
    let op = journal.claim(Claim::Deploy(empty)).unwrap();
    journal.seal(op, Vec::new()).unwrap();
    journal
        .set_phase(op, Phase::Prepared, Phase::Applying)
        .unwrap();

    let status = recover_pending(&mut journal, &mut effects, &root(1), &root(2))
        .unwrap()
        .unwrap();
    assert_eq!(status.phase, Phase::RolledBack);
    assert_eq!(journal.root_state(&root(2)).unwrap().pending, None);

    let plan = save_plan(&mut journal);
    assert!(journal.claim(Claim::Deploy(plan)).is_ok());
}
