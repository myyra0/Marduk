use std::cell::Cell;
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind, panic_any, resume_unwind};
use std::rc::Rc;

use marduk_core::deployment::*;
use marduk_core::ids::{InstalledRevisionId, MemberId, ProfileId, ProfileRevision};
use marduk_core::profiles::{ProfileMember, ProfileSnapshot};
use marduk_core::recovery::*;
use marduk_core::resolve::ResolvedInputs;
use marduk_core::root_identity::{
    FileMetadata, Fingerprint, NativeIdentity, RootError, RootIdentity,
};
use marduk_core::targets::{RelativePath, RootRole, TargetFile};

const CREATED: &str = "textures/a-only.dds";
const WINNER: &str = "textures/winner.dds";
const MISSING: &str = "z-missing/winner.dds";
const KEEP: &str = "untouched.txt";
const OUTSIDE: &str = "outside-plan.txt";
const BASE: &[u8] = b"base\n";
const ONLY_A: &[u8] = b"only-a\n";
const A: &[u8] = b"a\n";
const B: &[u8] = b"b\n";
const EDIT: &[u8] = b"edit\n";

fn digest(bytes: &[u8]) -> [u8; 32] {
    // Fixed SHA-256 vectors keep the core test dependency-free.
    let hex = match bytes {
        b"base\n" => "f34848ca92665c342abd5816c9e3eda0e82180671195362bcd0080544a3bc2ac",
        b"only-a\n" => "6b22cd9c94130e1b20ad174737b221124efee8023d3beb1fcb498390244682c3",
        b"b\n" => "0263829989b6fd954f72baaf2fc64bc2e2f01d692d4de72986ea808f6e99813f",
        b"a\n" => "87428fc522803d31065e7bce3cf03fe475096631e5e07bbd7a0fde60c4cf25c7",
        b"keep\n" => "f660a7996deacfbc7560e4240054a8ad82eb02fe25a95064257e07084bcacb85",
        b"user\n" => "6d9010b2b7a1483b256ae7477738dba7c530bd9ba53db1d6691441e74b83608a",
        b"edit\n" => "b348ed8c3dfbb87f42fcdc93633cc3915a3b70ce523d7f033272557775f5cdf9",
        _ => panic!("missing test digest"),
    };
    std::array::from_fn(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap())
}

fn identity(volume: u8, ino: u64) -> NativeIdentity {
    NativeIdentity::Linux {
        fs_magic: 0xef53,
        fs_id: [volume; 16],
        view_id: u64::from(volume),
        tree_id: None,
        ino,
        birth: (100, 0),
    }
}

fn root(volume: u8, ino: u64) -> RootIdentity {
    RootIdentity::from_adapter_evidence(identity(volume, ino))
}

fn target(path: &str) -> TargetFile {
    TargetFile {
        root: RootRole::Data,
        relative: RelativePath::parse(path).unwrap(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct File {
    bytes: &'static [u8],
    fingerprint: Fingerprint,
}

impl File {
    fn new(bytes: &'static [u8], ino: u64) -> Self {
        Self {
            bytes,
            fingerprint: Fingerprint {
                identity: identity(1, ino),
                len: bytes.len() as u64,
                sha256: digest(bytes),
                metadata: FileMetadata::Linux {
                    mode: if bytes == BASE { 0o100640 } else { 0o100644 },
                    uid: 1000,
                    gid: 1000,
                    mtime: (200 + bytes.len() as i64, 17),
                    nlink: 1,
                },
            },
        }
    }
}

#[derive(Clone, Default)]
struct Namespace {
    public: BTreeMap<String, File>,
    staged: Vec<(StageRef, File)>,
    backups: Vec<(BackupRef, File)>,
}

fn lookup<K: PartialEq>(entries: &[(K, File)], key: K) -> Option<&File> {
    entries
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, file)| file)
}

fn take<K: PartialEq>(entries: &mut Vec<(K, File)>, key: K) -> Option<File> {
    entries
        .iter()
        .position(|(k, _)| *k == key)
        .map(|i| entries.remove(i).1)
}

fn artifact(file: Option<&File>) -> Artifact {
    file.map_or(Artifact::Absent, |f| {
        Artifact::Regular(f.fingerprint.clone())
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mutation {
    Create,
    Replace,
    Detach,
}

struct Effects {
    visible: Namespace,
    durable: Namespace,
    sources: BTreeMap<(InstalledRevisionId, String), &'static [u8]>,
    volume: u8,
    extra_parent_exists: bool,
    revision: ProfileRevision,
    next_ino: u64,
    root_gate: Rc<Cell<bool>>,
    reads: usize,
    prepares: usize,
    mutations: Vec<(Mutation, String)>,
    prepared: Vec<PreparedStep>,
    backup_receipts: Vec<(BackupRef, File)>,
    barrier_fault: Option<(&'static str, bool)>,
}

impl Effects {
    fn new() -> Self {
        let visible = Namespace {
            public: BTreeMap::from([
                (WINNER.into(), File::new(BASE, 10)),
                (KEEP.into(), File::new(b"keep\n", 11)),
                (OUTSIDE.into(), File::new(b"user\n", 12)),
            ]),
            ..Namespace::default()
        };
        Self {
            durable: visible.clone(),
            visible,
            sources: BTreeMap::from([
                ((InstalledRevisionId::new(1), CREATED.into()), ONLY_A),
                ((InstalledRevisionId::new(1), WINNER.into()), A),
                ((InstalledRevisionId::new(1), MISSING.into()), A),
                ((InstalledRevisionId::new(2), WINNER.into()), B),
                ((InstalledRevisionId::new(2), MISSING.into()), B),
            ]),
            volume: 1,
            extra_parent_exists: false,
            revision: ProfileRevision::new(7),
            next_ino: 100,
            root_gate: Rc::default(),
            reads: 0,
            prepares: 0,
            mutations: Vec::new(),
            prepared: Vec::new(),
            backup_receipts: Vec::new(),
            barrier_fault: None,
        }
    }

    fn boot(&mut self) {
        self.visible = self.durable.clone();
        self.root_gate.set(false);
        self.prepared.clear();
    }

    fn external_write(&mut self, path: &str, bytes: &'static [u8]) {
        let file = File::new(bytes, self.next_ino);
        self.next_ino += 1;
        self.visible.public.insert(path.into(), file.clone());
        self.durable.public.insert(path.into(), file);
    }

    fn copy_file(&mut self, mut file: File) -> File {
        file.fingerprint.identity = identity(1, self.next_ino);
        self.next_ino += 1;
        file
    }

    fn check_target(&self, target: &BoundTarget) -> Result<(), RootError> {
        if self.volume != 1 {
            return Err(RootError::TargetUnavailable);
        }
        if !self.extra_parent_exists && target.target.relative.as_str() == MISSING {
            return Err(RootError::MissingParent);
        }
        assert_eq!(
            target.parent,
            root(
                1,
                if target.target.relative.as_str() == MISSING {
                    4
                } else {
                    3
                }
            )
        );
        assert_eq!(target.leaf, leaf(target.target.relative.as_str()));
        Ok(())
    }

    fn store_backup(&mut self, at: BackupRef, file: File) {
        assert!(
            lookup(&self.visible.backups, at).is_none(),
            "snapshot overwritten"
        );
        self.backup_receipts.push((at, file.clone()));
        self.visible.backups.push((at, file.clone()));
        self.durable.backups.push((at, file));
    }

    fn mutate(
        &mut self,
        kind: Mutation,
        target: &BoundTarget,
        expected: Option<&Fingerprint>,
        stage: Option<StageRef>,
        retain: Option<BackupRef>,
    ) -> EffectOutcome {
        let path = target.target.relative.as_str();
        self.mutations.push((kind, path.into()));
        if let Err(error) = self.check_target(target) {
            return EffectOutcome::Unavailable(error);
        }
        let step = self
            .prepared
            .iter()
            .find(|step| {
                step.spec.target.target.relative.as_str() == path
                    && match stage {
                        Some(at) => step.staged.as_ref().is_some_and(|(s, _)| *s == at),
                        None => retain == Some(step.retain),
                    }
            })
            .expect("effect without an owned prepared step")
            .clone();
        assert_eq!(
            retain,
            if kind == Mutation::Create {
                None
            } else {
                Some(step.retain)
            }
        );
        if self.visible.public.get(path).map(|f| &f.fingerprint) != expected {
            return EffectOutcome::Conflict(self.inspect_step(&step));
        }
        assert_eq!(
            expected,
            match &step.spec.pre {
                PreState::Absent => None,
                PreState::Regular(f) => Some(f),
            }
        );
        if let Some(at) = retain {
            assert!(lookup(&self.visible.backups, at).is_none());
            self.visible
                .backups
                .push((at, self.visible.public.remove(path).unwrap()));
        }
        if let Some(at) = stage {
            let output = take(&mut self.visible.staged, at).expect("missing stage");
            self.visible.public.insert(path.into(), output);
        }
        let evidence = self.inspect_step(&step);
        match retain {
            Some(at) => EffectOutcome::Displaced { evidence, at },
            None => EffectOutcome::Observed(evidence),
        }
    }
}

fn leaf(path: &str) -> &str {
    path.rsplit('/').next().unwrap()
}

struct Gate(Rc<Cell<bool>>);

impl Drop for Gate {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

impl DeploymentInputs for Effects {
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
        if *installation == root(self.volume, 1) && *data == root(self.volume, 2) {
            Ok(())
        } else {
            Err(DeployError::TargetUnavailable)
        }
    }

    fn resolve_target(&mut self, target: &TargetFile) -> Result<BoundTarget, RootError> {
        let bound = BoundTarget {
            target: target.clone(),
            parent: root(
                1,
                if target.relative.as_str() == MISSING {
                    4
                } else {
                    3
                },
            ),
            leaf: leaf(target.relative.as_str()).into(),
        };
        self.check_target(&bound)?;
        Ok(bound)
    }

    fn profile_revision(&mut self, profile: ProfileId) -> Result<ProfileRevision, DeployError> {
        assert_eq!(profile, ProfileId::new(1));
        Ok(self.revision)
    }

    fn source(
        &mut self,
        installed: InstalledRevisionId,
        path: &RelativePath,
    ) -> Result<SourceRef, DeployError> {
        let bytes = *self
            .sources
            .get(&(installed, path.as_str().into()))
            .ok_or(DeployError::StalePlan)?;
        Ok(SourceRef {
            installed,
            path: path.clone(),
            sha256: digest(bytes),
            len: bytes.len() as u64,
        })
    }
}

impl CopyEffects for Effects {
    fn observe(&mut self, target: &BoundTarget) -> Observation {
        if let Err(error) = self.check_target(target) {
            return Observation::Unavailable(error);
        }
        self.reads += 1;
        self.visible
            .public
            .get(target.target.relative.as_str())
            .map_or(Observation::Absent, |f| {
                Observation::Regular(f.fingerprint.clone())
            })
    }

    fn prepare(
        &mut self,
        op: OperationId,
        ordinal: u32,
        direction: StepDirection,
        intent: &StepSpec,
    ) -> Result<PreparedStep, DeployError> {
        self.prepares += 1;
        assert!(self.root_gate.get());
        let path = intent.target.target.relative.as_str();
        let current = match (&intent.pre, self.visible.public.get(path)) {
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
                let copy = self.copy_file(file.clone());
                self.store_backup(at, copy);
                at
            });
        }
        let step_pre = PreImage {
            observation: intent.pre.clone(),
            backup: current.map(|file| {
                let at = BackupRef::from_record(op, ordinal, direction, BackupSlot::Step);
                let copy = self.copy_file(file);
                self.store_backup(at, copy);
                at
            }),
        };
        let staged_file = match &spec.output {
            StepOutput::Absent => None,
            StepOutput::Source(source) => {
                let got = self.source(source.installed, &source.path)?;
                assert_eq!((source.sha256, source.len), (got.sha256, got.len));
                Some(File::new(
                    self.sources[&(source.installed, source.path.as_str().into())],
                    0,
                ))
            }
            StepOutput::Original(at) => {
                let receipt =
                    lookup(&self.backup_receipts, *at).ok_or(DeployError::RecoveryConflict)?;
                let file =
                    lookup(&self.visible.backups, *at).ok_or(DeployError::RecoveryConflict)?;
                if file != receipt {
                    return Err(DeployError::RecoveryConflict);
                }
                Some(file.clone())
            }
        };
        let staged = staged_file.map(|file| {
            let at = StageRef::from_record(op, ordinal, direction);
            let file = self.copy_file(file);
            self.visible.staged.push((at, file.clone()));
            self.durable.staged.push((at, file.clone()));
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
        self.prepared.push(step.clone());
        Ok(step)
    }

    fn validate_backup(
        &mut self,
        at: BackupRef,
        expected: &Fingerprint,
    ) -> Result<(), DeployError> {
        let file = lookup(&self.visible.backups, at).ok_or(DeployError::RecoveryConflict)?;
        let receipt = lookup(&self.backup_receipts, at).ok_or(DeployError::RecoveryConflict)?;
        let f = &file.fingerprint;
        // The independent snapshot's own identity is bound to its slot, not
        // to the recorded public pre-image inode.
        if file == receipt
            && (f.sha256, f.len, f.metadata) == (expected.sha256, expected.len, expected.metadata)
        {
            Ok(())
        } else {
            Err(DeployError::RecoveryConflict)
        }
    }

    fn publish_new_absent(&mut self, target: &BoundTarget, staged: StageRef) -> EffectOutcome {
        self.mutate(Mutation::Create, target, None, Some(staged), None)
    }

    fn replace_existing_retain_displaced(
        &mut self,
        target: &BoundTarget,
        expected: &Fingerprint,
        staged: StageRef,
        retain: BackupRef,
    ) -> EffectOutcome {
        self.mutate(
            Mutation::Replace,
            target,
            Some(expected),
            Some(staged),
            Some(retain),
        )
    }

    fn detach_existing_retain(
        &mut self,
        target: &BoundTarget,
        expected: &Fingerprint,
        retain: BackupRef,
    ) -> EffectOutcome {
        self.mutate(Mutation::Detach, target, Some(expected), None, Some(retain))
    }

    fn inspect_step(&mut self, step: &PreparedStep) -> Evidence {
        // A reopened fake recognizes the persisted receipt when a later
        // recovery must execute a still-ready step; it does not re-prepare it.
        if !self
            .prepared
            .iter()
            .any(|s| (s.op, s.ordinal, s.direction) == (step.op, step.ordinal, step.direction))
        {
            self.prepared.push(step.clone());
        }
        let target = self.observe(&step.spec.target);
        if matches!(target, Observation::Unavailable(_)) {
            return Evidence {
                target,
                stage: Artifact::Unavailable,
                retained: Artifact::Unavailable,
            };
        }
        Evidence {
            target,
            stage: artifact(
                step.staged
                    .as_ref()
                    .and_then(|(at, _)| lookup(&self.visible.staged, *at)),
            ),
            retained: artifact(lookup(&self.visible.backups, step.retain)),
        }
    }

    fn durability_barrier(&mut self, step: &PreparedStep, _: &Evidence) -> BarrierOutcome {
        let evidence = self.inspect_step(step);
        let path = step.spec.target.target.relative.as_str();
        if self.barrier_fault.is_some_and(|(at, _)| at == path) {
            if self.barrier_fault.take().unwrap().1 {
                self.durable = self.visible.clone();
            }
            return BarrierOutcome::Ambiguous(evidence);
        }
        self.durable = self.visible.clone();
        BarrierOutcome::Durable(evidence)
    }
}

#[derive(Clone, Default)]
struct Db {
    plans: BTreeMap<PlanId, DeploymentPlan>,
    claims: BTreeMap<PlanId, OperationId>,
    statuses: BTreeMap<OperationId, OperationStatus>,
    steps: BTreeMap<OperationId, Vec<StepRecord>>,
    pending: Option<OperationId>,
    active: Option<GenerationId>,
    generations: BTreeMap<GenerationId, OperationId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cut {
    BeforeFirstPublish,
    BeforeWinnerAck,
}

#[derive(Default)]
struct Ledger {
    durable: Db,
    visible: Db,
    cut: Option<Cut>,
    lose_finish_ack: bool,
    disconnected: bool,
    reopen_count: usize,
    finishes: usize,
}

impl Ledger {
    fn persist(&mut self) {
        assert!(
            !self.disconnected,
            "journal reused after lost connection without reopen"
        );
        self.durable = self.visible.clone();
    }

    fn boot(&mut self) {
        self.visible = self.durable.clone();
        self.disconnected = false;
        self.cut = None;
    }
}

impl Journal for Ledger {
    fn reopen(&mut self) -> Result<(), DeployError> {
        self.reopen_count += 1;
        self.boot();
        Ok(())
    }

    fn save_plan(
        &mut self,
        profile: ProfileId,
        revision: ProfileRevision,
        installation: RootIdentity,
        data: RootIdentity,
        intents: Vec<FileIntent>,
    ) -> Result<DeploymentPlan, DeployError> {
        let id = PlanId::try_from_u64(self.visible.plans.len() as u64 + 1)?;
        let plan = DeploymentPlan {
            id,
            profile,
            revision,
            installation,
            data,
            intents,
        };
        self.visible.plans.insert(id, plan.clone());
        self.persist();
        Ok(plan)
    }

    fn plan(&mut self, id: PlanId) -> Result<Option<DeploymentPlan>, DeployError> {
        Ok(self.visible.plans.get(&id).cloned())
    }

    fn claimed(&mut self, id: PlanId) -> Result<Option<OperationStatus>, DeployError> {
        assert!(!self.disconnected, "claim inspected without reopen");
        Ok(self
            .visible
            .claims
            .get(&id)
            .map(|op| self.visible.statuses[op].clone()))
    }

    fn root_state(&mut self, data: &RootIdentity) -> Result<RootState, DeployError> {
        assert_eq!(*data, root(1, 2));
        assert!(!self.disconnected, "lease inspected without reopen");
        Ok(RootState {
            pending: self.visible.pending,
            active: self.visible.active,
        })
    }

    fn claim(&mut self, claim: Claim) -> Result<OperationId, DeployError> {
        if let Claim::Deploy(plan) = &claim {
            if let Some(op) = self.visible.claims.get(&plan.id) {
                return Ok(*op);
            }
        }
        if self.visible.pending.is_some() {
            return Err(DeployError::Busy);
        }
        let id = OperationId::try_from_u64(self.visible.statuses.len() as u64 + 1)?;
        let (kind, generation) = match claim {
            Claim::Deploy(plan) => {
                if self.visible.active.is_some() {
                    return Err(DeployError::ActiveDeployment);
                }
                self.visible.claims.insert(plan.id, id);
                (OperationKind::Deploy, None)
            }
            Claim::Restore {
                installation,
                data,
                generation,
            } => {
                assert_eq!((installation, data), (root(1, 1), root(1, 2)));
                assert_eq!(self.visible.active, Some(generation));
                (OperationKind::Restore, Some(generation))
            }
        };
        self.visible.pending = Some(id);
        self.visible.statuses.insert(
            id,
            OperationStatus {
                id,
                kind,
                generation,
                phase: Phase::Preparing,
                blocker: None,
                retained: Vec::new(),
            },
        );
        self.persist();
        Ok(id)
    }

    fn seal(&mut self, op: OperationId, steps: Vec<PreparedStep>) -> Result<(), DeployError> {
        let status = self.visible.statuses.get_mut(&op).unwrap();
        assert_eq!(status.phase, Phase::Preparing);
        assert_eq!(self.visible.pending, Some(op));
        assert!(!steps.is_empty());
        status.phase = Phase::Prepared;
        status.retained = steps
            .iter()
            .flat_map(|s| [s.spec.original.backup, s.step_pre.backup])
            .flatten()
            .collect();
        assert!(
            self.visible
                .steps
                .insert(
                    op,
                    steps
                        .into_iter()
                        .map(|prepared| StepRecord {
                            prepared,
                            phase: StepPhase::Ready,
                            evidence: None,
                            undo: None,
                        })
                        .collect()
                )
                .is_none()
        );
        self.persist();
        Ok(())
    }

    fn set_phase(&mut self, op: OperationId, from: Phase, to: Phase) -> Result<(), DeployError> {
        let status = self.visible.statuses.get_mut(&op).unwrap();
        assert_eq!(status.phase, from);
        assert_eq!(self.visible.pending, Some(op));
        status.phase = to;
        self.persist();
        Ok(())
    }

    fn record_step(
        &mut self,
        op: OperationId,
        ordinal: u32,
        from: StepPhase,
        to: StepPhase,
        evidence: Option<Evidence>,
    ) -> Result<(), DeployError> {
        assert_eq!(self.visible.pending, Some(op));
        let row = self
            .visible
            .steps
            .get_mut(&op)
            .unwrap()
            .iter_mut()
            .find(|s| s.prepared.ordinal == ordinal)
            .unwrap();
        let path = row.prepared.spec.target.target.relative.as_str();
        let lose_row =
            self.cut == Some(Cut::BeforeWinnerAck) && path == WINNER && to == StepPhase::Applied;
        let before_publish = self.cut == Some(Cut::BeforeFirstPublish)
            && path == CREATED
            && to == StepPhase::Attempting;
        assert_eq!(row.phase, from);
        row.phase = to;
        assert!(
            to != StepPhase::Applied || evidence.is_some(),
            "applied without evidence"
        );
        if evidence.is_some() {
            row.evidence = evidence;
        }
        if lose_row {
            panic_any(self.cut.take().unwrap()); // visible SQL change, never committed
        }
        self.persist();
        if before_publish {
            panic_any(self.cut.take().unwrap());
        }
        Ok(())
    }

    fn block(&mut self, op: OperationId, error: Option<DeployError>) -> Result<(), DeployError> {
        self.visible.statuses.get_mut(&op).unwrap().blocker = error;
        self.persist();
        Ok(())
    }

    fn finish(&mut self, op: OperationId, how: Finalize) -> Result<OperationStatus, DeployError> {
        self.finishes += 1;
        assert_eq!(self.visible.pending, Some(op));
        let status = self.visible.statuses.get_mut(&op).unwrap();
        status.phase = match how {
            Finalize::DeployCommitted => {
                assert_eq!(status.kind, OperationKind::Deploy);
                assert_eq!(status.phase, Phase::Applying);
                assert!(status.blocker.is_none());
                assert!(
                    self.visible.steps[&op]
                        .iter()
                        .all(|s| s.phase == StepPhase::Applied)
                );
                let id = GenerationId::try_from_u64(self.visible.generations.len() as u64 + 1)?;
                self.visible.generations.insert(id, op);
                self.visible.active = Some(id);
                status.generation = Some(id);
                Phase::Completed
            }
            Finalize::RestoreCompleted => {
                assert_eq!(status.kind, OperationKind::Restore);
                assert_eq!(status.phase, Phase::Restoring);
                assert!(status.blocker.is_none());
                assert!(
                    self.visible.steps[&op]
                        .iter()
                        .all(|s| s.phase == StepPhase::Applied)
                );
                self.visible.active = None;
                status.generation = None;
                Phase::Completed
            }
            Finalize::RolledBack => Phase::RolledBack,
            Finalize::Cancelled => Phase::Cancelled,
        };
        status.blocker = None;
        self.visible.pending = None;
        let status = status.clone();
        self.persist();
        if std::mem::take(&mut self.lose_finish_ack) {
            self.disconnected = true;
            return Err(DeployError::Io);
        }
        Ok(status)
    }

    fn status(&mut self, op: OperationId) -> Result<Option<OperationStatus>, DeployError> {
        assert!(!self.disconnected, "status inspected without reopen");
        Ok(self.visible.statuses.get(&op).cloned())
    }

    fn steps(&mut self, op: OperationId) -> Result<Vec<StepRecord>, DeployError> {
        Ok(self.visible.steps.get(&op).cloned().unwrap_or_default())
    }

    fn generation(&mut self, id: GenerationId) -> Result<GenerationRecord, DeployError> {
        let deploy = self.visible.generations[&id];
        Ok(GenerationRecord {
            id,
            deploy,
            steps: self.visible.steps[&deploy].clone(),
        })
    }

    fn record_undo(
        &mut self,
        op: OperationId,
        ordinal: u32,
        undo: PreparedStep,
    ) -> Result<(), DeployError> {
        assert_eq!(self.visible.pending, Some(op));
        let row = self
            .visible
            .steps
            .get_mut(&op)
            .unwrap()
            .iter_mut()
            .find(|s| s.prepared.ordinal == ordinal)
            .unwrap();
        assert_eq!(undo.direction, StepDirection::Undo);
        assert_ne!(undo.retain, row.prepared.retain);
        assert!(row.undo.replace(undo).is_none());
        self.persist();
        Ok(())
    }
}

struct Fixture {
    journal: Ledger,
    effects: Effects,
    profile: ProfileSnapshot,
    inputs: ResolvedInputs,
}

impl Fixture {
    fn new(winner: &str) -> Self {
        Self {
            journal: Ledger::default(),
            effects: Effects::new(),
            profile: ProfileSnapshot {
                id: ProfileId::new(1),
                revision: ProfileRevision::new(7),
                members: vec![
                    ProfileMember {
                        id: MemberId::new(1),
                        installed: InstalledRevisionId::new(1),
                        enabled: true,
                    },
                    ProfileMember {
                        id: MemberId::new(2),
                        installed: InstalledRevisionId::new(2),
                        enabled: true,
                    },
                ],
            },
            inputs: ResolvedInputs {
                manifests: BTreeMap::from([
                    (
                        InstalledRevisionId::new(1),
                        vec![target(winner), target(CREATED)],
                    ),
                    (InstalledRevisionId::new(2), vec![target(winner)]),
                ]),
            },
        }
    }

    fn preview(&mut self) -> Result<DeploymentPlan, DeployError> {
        preview_deployment(
            &mut self.journal,
            &mut self.effects,
            &self.profile,
            &self.inputs,
            &root(1, 1),
            &root(1, 2),
        )
    }

    fn plan(&mut self) -> PlanId {
        let plan = self.preview().unwrap();
        assert_eq!(
            plan.intents
                .iter()
                .map(|i| i.target.target.relative.as_str())
                .collect::<Vec<_>>(),
            [CREATED, WINNER]
        );
        assert_eq!(
            plan.intents[1].source.installed,
            InstalledRevisionId::new(2)
        );
        assert!(matches!(plan.intents[0].original, PreState::Absent));
        assert!(
            matches!(&plan.intents[1].original, PreState::Regular(f) if *f == File::new(BASE, 10).fingerprint)
        );
        plan.id
    }

    fn commit(&mut self, plan: PlanId) -> Result<OperationStatus, DeployError> {
        commit_deployment(&mut self.journal, &mut self.effects, plan, || false)
    }

    fn deploy(&mut self) -> OperationStatus {
        let id = self.plan();
        let status = self.commit(id).unwrap();
        self.assert_status(&status, OperationKind::Deploy, Phase::Completed, None);
        self.root_state(None, Some(status.generation.unwrap()));
        self.files(Some(ONLY_A), B);
        self.mutations(&[(Mutation::Create, CREATED), (Mutation::Replace, WINNER)]);
        status
    }

    fn restore(&mut self) -> Result<OperationStatus, DeployError> {
        restore(
            &mut self.journal,
            &mut self.effects,
            &root(1, 1),
            &root(1, 2),
        )
    }

    fn recover(&mut self) -> Option<OperationStatus> {
        recover_pending(
            &mut self.journal,
            &mut self.effects,
            &root(1, 1),
            &root(1, 2),
        )
        .unwrap()
    }

    fn state(&mut self, id: OperationId) -> OperationStatus {
        operation_status(&mut self.journal, id).unwrap()
    }

    fn cut_apply(&mut self, plan: PlanId, cut: Cut) -> OperationId {
        self.journal.cut = Some(cut);
        interrupted(cut, || {
            let _ = self.commit(plan);
        });
        self.journal.durable.pending.unwrap()
    }

    fn cut_restore(&mut self) -> OperationId {
        self.journal.cut = Some(Cut::BeforeWinnerAck);
        interrupted(Cut::BeforeWinnerAck, || {
            let _ = self.restore();
        });
        self.journal.durable.pending.unwrap()
    }

    fn boot(&mut self) {
        self.journal.boot();
        self.effects.boot();
    }

    fn root_state(&mut self, pending: Option<OperationId>, active: Option<GenerationId>) {
        let current = self.journal.root_state(&root(1, 2)).unwrap();
        assert_eq!((current.pending, current.active), (pending, active));
        assert_eq!(
            (self.journal.durable.pending, self.journal.durable.active),
            (pending, active)
        );
    }

    fn assert_status(
        &mut self,
        status: &OperationStatus,
        kind: OperationKind,
        phase: Phase,
        blocker: Option<DeployError>,
    ) {
        assert_eq!(
            (status.kind, status.phase, status.blocker),
            (kind, phase, blocker)
        );
        let persisted = self.state(status.id);
        assert_eq!(
            (
                persisted.kind,
                persisted.phase,
                persisted.blocker,
                persisted.generation,
                persisted.retained
            ),
            (
                kind,
                phase,
                blocker,
                status.generation,
                status.retained.clone()
            ),
        );
    }

    fn files(&self, created: Option<&[u8]>, winner: &[u8]) {
        for ns in [&self.effects.visible, &self.effects.durable] {
            assert_eq!(ns.public.len(), if created.is_some() { 4 } else { 3 });
            assert_eq!(ns.public.get(CREATED).map(|f| f.bytes), created);
            let file = &ns.public[WINNER];
            assert_eq!(file.bytes, winner);
            assert_eq!(
                file.fingerprint.metadata,
                File::new(file.bytes, 0).fingerprint.metadata
            );
            assert_eq!(ns.public[KEEP], File::new(b"keep\n", 11));
            assert_eq!(ns.public[OUTSIDE], File::new(b"user\n", 12));
        }
        assert_eq!(
            self.effects.sources[&(InstalledRevisionId::new(1), CREATED.into())],
            ONLY_A
        );
        assert_eq!(
            self.effects.sources[&(InstalledRevisionId::new(1), WINNER.into())],
            A
        );
        assert_eq!(
            self.effects.sources[&(InstalledRevisionId::new(2), WINNER.into())],
            B
        );
    }

    fn mutations(&self, expected: &[(Mutation, &str)]) {
        assert_eq!(
            self.effects
                .mutations
                .iter()
                .map(|(m, p)| (*m, p.as_str()))
                .collect::<Vec<_>>(),
            expected
        );
    }

    fn steps(&self, op: OperationId, expected: &[(&str, StepPhase)]) {
        let rows = &self.journal.durable.steps[&op];
        assert_eq!(rows.len(), expected.len());
        // The journal keys by canonical ordinal; a Restore may return records
        // in either storage order, but public effects must run in reverse.
        for (path, phase) in expected {
            let row = rows
                .iter()
                .find(|s| s.prepared.spec.target.target.relative.as_str() == *path)
                .unwrap();
            assert_eq!(row.phase, *phase);
            if *phase == StepPhase::Applied {
                assert!(row.evidence.is_some(), "missing step observation");
            }
        }
    }

    fn original_backup(&self, op: OperationId) -> BackupRef {
        let row = self.journal.durable.steps[&op]
            .iter()
            .find(|s| s.prepared.spec.target.target.relative.as_str() == WINNER)
            .unwrap();
        let backup = row.prepared.spec.original.backup.unwrap();
        let step = row.prepared.step_pre.backup.unwrap();
        assert_ne!(backup, row.prepared.retain);
        assert_ne!(step, row.prepared.retain);
        assert!(
            matches!(&row.prepared.spec.original.observation, PreState::Regular(f) if *f == File::new(BASE, 10).fingerprint)
        );
        let copy = lookup(&self.effects.durable.backups, backup).unwrap();
        assert_eq!(copy.bytes, BASE);
        assert_eq!(
            copy.fingerprint.metadata,
            File::new(BASE, 10).fingerprint.metadata
        );
        assert_ne!(
            copy.fingerprint.identity,
            identity(1, 10),
            "backup must not alias public bytes"
        );
        backup
    }

    fn assert_resolved(
        &mut self,
        op: OperationId,
        kind: OperationKind,
        phase: Phase,
        generation: Option<GenerationId>,
    ) {
        let status = self
            .recover()
            .expect("pending operation was not reconciled");
        assert_eq!((status.id, status.generation), (op, generation));
        self.assert_status(&status, kind, phase, None);
        self.root_state(None, None);
        let count = self.effects.mutations.len();
        assert!(self.recover().is_none());
        assert_eq!(self.effects.mutations.len(), count);
    }
}

fn interrupted(cut: Cut, call: impl FnOnce()) {
    match catch_unwind(AssertUnwindSafe(call)) {
        Err(error) if error.downcast_ref::<Cut>() == Some(&cut) => {}
        Err(error) => resume_unwind(error),
        Ok(()) => panic!("did not reach {cut:?}"),
    }
}

#[test]
fn cut_after_record_before_publish() {
    let mut f = Fixture::new(WINNER);
    let plan = f.plan();
    let op = f.cut_apply(plan, Cut::BeforeFirstPublish);
    f.root_state(Some(op), None);
    f.steps(
        op,
        &[(CREATED, StepPhase::Attempting), (WINNER, StepPhase::Ready)],
    );
    assert_eq!(f.state(op).phase, Phase::Applying);
    f.original_backup(op);
    f.files(None, BASE);
    f.mutations(&[]);
    f.boot();
    f.assert_resolved(op, OperationKind::Deploy, Phase::RolledBack, None);
    f.files(None, BASE);
    f.mutations(&[]);
}

#[test]
fn cut_after_publish_before_ack() {
    let mut f = Fixture::new(WINNER);
    let plan = f.plan();
    let op = f.cut_apply(plan, Cut::BeforeWinnerAck);
    f.root_state(Some(op), None);
    f.steps(
        op,
        &[
            (CREATED, StepPhase::Applied),
            (WINNER, StepPhase::Attempting),
        ],
    );
    assert_eq!(f.state(op).phase, Phase::Applying);
    f.original_backup(op);
    f.files(Some(ONLY_A), B);
    f.boot();
    f.assert_resolved(op, OperationKind::Deploy, Phase::RolledBack, None);
    f.files(None, BASE);
    f.mutations(&[
        (Mutation::Create, CREATED),
        (Mutation::Replace, WINNER),
        (Mutation::Replace, WINNER),
        (Mutation::Detach, CREATED),
    ]);

    f.steps(
        op,
        &[(CREATED, StepPhase::Undone), (WINNER, StepPhase::Undone)],
    );
    // Replaying even a stale, now-unavailable plan returns the terminal claim.
    f.effects.revision = ProfileRevision::new(8);
    f.effects.volume = 2;
    let status = f.commit(plan).unwrap();
    assert_eq!(
        (status.id, status.phase, status.generation, status.blocker),
        (op, Phase::RolledBack, None, None)
    );
    f.root_state(None, None);
    assert_eq!(f.effects.mutations.len(), 4);
}

#[test]
fn failed_final_barrier_no_success() {
    for survives in [false, true] {
        let mut f = Fixture::new(WINNER);
        let plan = f.plan();
        f.effects.barrier_fault = Some((WINNER, survives));
        assert!(matches!(f.commit(plan), Err(DeployError::Io)));
        let op = f.journal.durable.pending.unwrap();
        let status = f.state(op);
        assert_eq!(
            (status.phase, status.generation, status.blocker),
            (Phase::Applying, None, Some(DeployError::Io))
        );
        assert_eq!(f.journal.finishes, 0);
        f.root_state(Some(op), None);
        f.steps(
            op,
            &[
                (CREATED, StepPhase::Applied),
                (WINNER, StepPhase::Attempting),
            ],
        );
        assert_eq!(f.effects.visible.public[WINNER].bytes, B);
        assert_eq!(
            f.effects.durable.public[WINNER].bytes,
            if survives { B } else { BASE }
        );
        f.original_backup(op);
        f.boot();
        f.assert_resolved(op, OperationKind::Deploy, Phase::RolledBack, None);
        f.files(None, BASE);
        let mut want = vec![(Mutation::Create, CREATED), (Mutation::Replace, WINNER)];
        if survives {
            want.push((Mutation::Replace, WINNER));
        }
        want.push((Mutation::Detach, CREATED));
        f.mutations(&want);
    }
}

#[test]
fn committed_before_reply_does_not_reapply() {
    let mut f = Fixture::new(WINNER);
    let plan = f.plan();
    f.journal.lose_finish_ack = true;
    let committed = f
        .commit(plan)
        .expect("lost SQL ACK is resolved by reopening and inspecting");
    assert_eq!(f.journal.reopen_count, 1);
    assert_eq!(f.journal.finishes, 1);
    f.assert_status(&committed, OperationKind::Deploy, Phase::Completed, None);
    let generation = committed.generation.unwrap();
    f.root_state(None, Some(generation));
    f.steps(
        committed.id,
        &[(CREATED, StepPhase::Applied), (WINNER, StepPhase::Applied)],
    );
    f.original_backup(committed.id);
    f.files(Some(ONLY_A), B);
    f.boot(); // caller never saw the response; retry across a restart
    f.effects.revision = ProfileRevision::new(8);
    f.effects.volume = 2;
    let replay = f.commit(plan).unwrap();
    assert_eq!(
        (replay.id, replay.phase, replay.generation),
        (committed.id, Phase::Completed, Some(generation))
    );
    f.root_state(None, Some(generation));
    assert!(f.recover().is_none());
    assert_eq!(f.journal.finishes, 1);
    f.mutations(&[(Mutation::Create, CREATED), (Mutation::Replace, WINNER)]);
    f.files(Some(ONLY_A), B);
}

#[test]
fn restore_twice_is_noop() {
    let mut f = Fixture::new(WINNER);
    let deploy = f.deploy();
    let original = f.original_backup(deploy.id);
    f.effects.revision = ProfileRevision::new(8);
    let restore = f.cut_restore();
    f.root_state(Some(restore), deploy.generation);
    f.steps(
        restore,
        &[(CREATED, StepPhase::Ready), (WINNER, StepPhase::Attempting)],
    );
    assert_eq!(f.state(restore).phase, Phase::Restoring);
    let row = f.journal.durable.steps[&restore]
        .iter()
        .find(|r| r.prepared.spec.target.target.relative.as_str() == WINNER)
        .unwrap();
    assert_eq!(row.prepared.spec.original.backup, Some(original));
    let step_backup = row.prepared.step_pre.backup.unwrap();
    let displaced = row.prepared.retain;
    assert_ne!(original, step_backup);
    assert_ne!(displaced, step_backup);
    assert_eq!(
        lookup(&f.effects.durable.backups, step_backup)
            .unwrap()
            .bytes,
        B
    );
    assert_eq!(
        lookup(&f.effects.durable.backups, displaced).unwrap().bytes,
        B
    );
    assert_ne!(
        lookup(&f.effects.durable.backups, step_backup)
            .unwrap()
            .fingerprint
            .identity,
        lookup(&f.effects.durable.backups, displaced)
            .unwrap()
            .fingerprint
            .identity
    );
    f.files(Some(ONLY_A), BASE);
    f.boot();
    f.assert_resolved(restore, OperationKind::Restore, Phase::Completed, None);
    f.steps(
        restore,
        &[(CREATED, StepPhase::Applied), (WINNER, StepPhase::Applied)],
    );
    assert!(matches!(f.restore(), Err(DeployError::NoActiveDeployment)));
    f.root_state(None, None);
    f.files(None, BASE);
    f.mutations(&[
        (Mutation::Create, CREATED),
        (Mutation::Replace, WINNER),
        (Mutation::Replace, WINNER),
        (Mutation::Detach, CREATED),
    ]);
}

#[test]
fn restart_preserves_external_edit_and_backup() {
    let mut f = Fixture::new(WINNER);
    let plan = f.plan();
    let op = f.cut_apply(plan, Cut::BeforeWinnerAck);
    f.effects.external_write(WINNER, EDIT);
    let external = f.effects.durable.public[WINNER].clone();
    f.boot();
    let original = f.original_backup(op);
    for _ in 0..2 {
        let status = f.recover().unwrap();
        assert_eq!((status.id, status.generation), (op, None));
        f.assert_status(
            &status,
            OperationKind::Deploy,
            Phase::RollingBack,
            Some(DeployError::RecoveryConflict),
        );
        assert!(status.retained.contains(&original));
        f.root_state(Some(op), None);
        f.files(Some(ONLY_A), EDIT);
        assert_eq!(f.effects.durable.public[WINNER], external);
        assert_eq!(f.original_backup(op), original);
        f.mutations(&[(Mutation::Create, CREATED), (Mutation::Replace, WINNER)]);
    }
}

#[test]
fn wrong_volume_waits() {
    let mut f = Fixture::new(WINNER);
    let deployment = f.deploy();
    let op = f.cut_restore();
    let original = f.original_backup(deployment.id);
    f.boot();
    f.effects.volume = 2;
    let reads = f.effects.reads;
    for _ in 0..2 {
        let status = f.recover().unwrap();
        assert_eq!((status.id, status.generation), (op, deployment.generation));
        f.assert_status(
            &status,
            OperationKind::Restore,
            Phase::Restoring,
            Some(DeployError::TargetUnavailable),
        );
        f.root_state(Some(op), deployment.generation);
        assert!(status.retained.contains(&original));
        assert_eq!(f.original_backup(deployment.id), original);
        assert_eq!(f.effects.reads, reads);
        f.files(Some(ONLY_A), BASE);
        assert_eq!(f.effects.mutations.len(), 3);
    }
    f.effects.volume = 1;
    f.assert_resolved(op, OperationKind::Restore, Phase::Completed, None);
    f.files(None, BASE);
    f.mutations(&[
        (Mutation::Create, CREATED),
        (Mutation::Replace, WINNER),
        (Mutation::Replace, WINNER),
        (Mutation::Detach, CREATED),
    ]);
}

#[test]
fn unknown_external_file_is_not_removed() {
    let mut f = Fixture::new(WINNER);
    let plan = f.plan();
    let op = f.cut_apply(plan, Cut::BeforeWinnerAck);
    let managed = f.effects.durable.public[CREATED].fingerprint.clone();
    f.effects.external_write(CREATED, ONLY_A);
    let external = f.effects.durable.public[CREATED].clone();
    assert_eq!(
        (external.fingerprint.sha256, external.fingerprint.metadata),
        (managed.sha256, managed.metadata)
    );
    assert_ne!(external.fingerprint.identity, managed.identity);
    f.boot();
    for _ in 0..2 {
        let status = f.recover().unwrap();
        assert_eq!((status.id, status.generation), (op, None));
        f.assert_status(
            &status,
            OperationKind::Deploy,
            Phase::RollingBack,
            Some(DeployError::RecoveryConflict),
        );
        f.root_state(Some(op), None);
        f.files(Some(ONLY_A), BASE);
        assert_eq!(f.effects.durable.public[CREATED], external);
        f.mutations(&[
            (Mutation::Create, CREATED),
            (Mutation::Replace, WINNER),
            (Mutation::Replace, WINNER),
        ]);
    }
}

#[test]
fn missing_parent_is_refused_before_effects() {
    let mut f = Fixture::new(MISSING);
    assert!(matches!(f.preview(), Err(DeployError::InvalidPath)));
    assert!(f.journal.durable.plans.is_empty());
    f.effects.extra_parent_exists = true;
    let plan = f.preview().unwrap();
    assert_eq!(plan.intents[0].target.target.relative.as_str(), CREATED);
    f.effects.extra_parent_exists = false;
    assert!(matches!(f.commit(plan.id), Err(DeployError::InvalidPath)));
    assert_eq!(f.effects.prepares, 0);
    f.root_state(None, None);
    f.files(None, BASE);
    f.mutations(&[]);
}

#[test]
fn unknown_original_is_refused_before_effects() {
    let mut f = Fixture::new(WINNER);
    let id = f.plan();
    f.effects.external_write(WINNER, EDIT);
    assert!(matches!(f.commit(id), Err(DeployError::RecoveryConflict)));
    assert_eq!(f.effects.prepares, 0);
    f.root_state(None, None);
    f.files(None, EDIT);
    f.mutations(&[]);
}

#[test]
fn cancelled_before_public_phase_has_no_effect_or_generation() {
    let mut f = Fixture::new(WINNER);
    let id = f.plan();
    let status = commit_deployment(&mut f.journal, &mut f.effects, id, || true).unwrap();
    f.assert_status(&status, OperationKind::Deploy, Phase::Cancelled, None);
    assert_eq!(status.generation, None);
    f.root_state(None, None);
    f.files(None, BASE);
    f.mutations(&[]);
    f.boot();
    assert!(f.recover().is_none());
    assert_eq!(f.commit(id).unwrap().phase, Phase::Cancelled);
    f.mutations(&[]);
}

#[test]
fn second_client_cannot_claim_pending_root() {
    let mut f = Fixture::new(WINNER);
    let first = f.plan();
    let second = f.plan();
    let op = f.cut_apply(first, Cut::BeforeFirstPublish);
    f.boot();
    assert!(matches!(f.commit(second), Err(DeployError::Busy)));
    assert!(f.journal.claimed(second).unwrap().is_none());
    f.root_state(Some(op), None);
    f.files(None, BASE);
    f.mutations(&[]);
}

#[test]
fn missing_or_corrupt_step_preimage_is_not_a_safe_rollback() {
    for corrupt in [false, true] {
        let mut f = Fixture::new(WINNER);
        let plan = f.plan();
        let op = f.cut_apply(plan, Cut::BeforeWinnerAck);
        let original = f.original_backup(op);
        let row = f.journal.durable.steps[&op]
            .iter()
            .find(|r| r.prepared.spec.target.target.relative.as_str() == WINNER)
            .unwrap();
        let at = row.prepared.step_pre.backup.unwrap();
        let _ = take(&mut f.effects.durable.backups, at).unwrap();
        if corrupt {
            f.effects.durable.backups.push((at, File::new(EDIT, 998)));
        }
        f.boot();
        for _ in 0..2 {
            let status = f.recover().unwrap();
            assert_eq!((status.id, status.generation), (op, None));
            f.assert_status(
                &status,
                OperationKind::Deploy,
                Phase::RollingBack,
                Some(DeployError::RecoveryConflict),
            );
            assert!(status.retained.contains(&original));
            f.root_state(Some(op), None);
            f.files(Some(ONLY_A), B);
            f.mutations(&[(Mutation::Create, CREATED), (Mutation::Replace, WINNER)]);
        }
    }
}
