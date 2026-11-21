//! A zero-step Restore interrupted with a persisted blocker must clear it and
//! release the root lease on the next recovery.

use std::path::Path;

use marduk_core::deployment::*;
use marduk_core::ids::{InstalledRevisionId, ProfileId, ProfileRevision};
use marduk_core::recovery::*;
use marduk_core::root_identity::{RootError, RootIdentity};
use marduk_core::targets::{RelativePath, TargetFile};
use marduk_storage::operation_journal::OperationJournal;
use rusqlite::Connection;
use tempfile::TempDir;

fn root(id: u8) -> RootIdentity {
    RootIdentity::from_adapter_evidence(marduk_core::root_identity::NativeIdentity::Windows {
        volume_serial: 71,
        file_id: [id; 16],
        creation_100ns: 201,
    })
}

fn fixture_v1() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let db = Connection::open(dir.path().join("catalog.sqlite3")).unwrap();
    db.execute_batch(include_str!("../migrations/0001_library.sql"))
        .unwrap();
    db.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA user_version = 1;
         INSERT INTO profiles (id, revision) VALUES (1, 7);",
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

fn empty_plan(journal: &mut OperationJournal) -> DeploymentPlan {
    journal
        .save_plan(
            ProfileId::new(1),
            ProfileRevision::new(7),
            root(1),
            root(2),
            Vec::new(),
        )
        .unwrap()
}

/// Zero-step recovery never invokes a target effect; any call is a bug.
struct NoEffects;

impl DeploymentInputs for NoEffects {
    type RootGuard = ();
    type LibraryGuard = ();

    fn try_root_gate(&mut self) -> Result<(), DeployError> {
        Ok(())
    }

    fn try_library_gate(&mut self) -> Result<(), DeployError> {
        Ok(())
    }

    fn verify_root(&mut self, _: &RootIdentity, _: &RootIdentity) -> Result<(), DeployError> {
        Ok(())
    }

    fn resolve_target(&mut self, _: &TargetFile) -> Result<BoundTarget, RootError> {
        panic!("resolved a target for a zero-step restore")
    }

    fn profile_revision(&mut self, _: ProfileId) -> Result<ProfileRevision, DeployError> {
        panic!("read a profile revision for a zero-step restore")
    }

    fn source(
        &mut self,
        _: InstalledRevisionId,
        _: &RelativePath,
    ) -> Result<SourceRef, DeployError> {
        panic!("read a source for a zero-step restore")
    }
}

impl CopyEffects for NoEffects {
    fn observe(&mut self, _: &BoundTarget) -> Observation {
        panic!("observed a target for a zero-step restore")
    }

    fn prepare(
        &mut self,
        _: OperationId,
        _: u32,
        _: StepDirection,
        _: &StepSpec,
    ) -> Result<PreparedStep, DeployError> {
        panic!("prepared an effect for a zero-step restore")
    }

    fn publish_new_absent(&mut self, _: &BoundTarget, _: StageRef) -> EffectOutcome {
        panic!("published for a zero-step restore")
    }

    fn replace_existing_retain_displaced(
        &mut self,
        _: &BoundTarget,
        _: &marduk_core::root_identity::Fingerprint,
        _: StageRef,
        _: BackupRef,
    ) -> EffectOutcome {
        panic!("replaced for a zero-step restore")
    }

    fn detach_existing_retain(
        &mut self,
        _: &BoundTarget,
        _: &marduk_core::root_identity::Fingerprint,
        _: BackupRef,
    ) -> EffectOutcome {
        panic!("detached for a zero-step restore")
    }

    fn inspect_step(&mut self, _: &PreparedStep) -> Evidence {
        panic!("inspected for a zero-step restore")
    }

    fn durability_barrier(&mut self, _: &PreparedStep, _: &Evidence) -> BarrierOutcome {
        panic!("barriered for a zero-step restore")
    }

    fn validate_backup(
        &mut self,
        _: BackupRef,
        _: &marduk_core::root_identity::Fingerprint,
    ) -> Result<(), DeployError> {
        panic!("validated a backup for a zero-step restore")
    }
}

#[test]
fn blocked_zero_step_restore_resolves_and_releases_the_lease() {
    let dir = fixture_v1();
    let mut journal = open_fixture(&dir);
    let mut effects = NoEffects;

    let empty = empty_plan(&mut journal);
    let deploy = journal.claim(Claim::Deploy(empty)).unwrap();
    journal.seal(deploy, Vec::new()).unwrap();
    journal
        .set_phase(deploy, Phase::Prepared, Phase::Applying)
        .unwrap();
    let generation = journal
        .finish(deploy, Finalize::DeployCommitted)
        .unwrap()
        .generation
        .unwrap();

    let restore = journal
        .claim(Claim::Restore {
            installation: root(1),
            data: root(2),
            generation,
        })
        .unwrap();
    journal.seal(restore, Vec::new()).unwrap();
    journal
        .set_phase(restore, Phase::Prepared, Phase::Restoring)
        .unwrap();
    journal
        .block(restore, Some(DeployError::TargetUnavailable))
        .unwrap();
    let blocked = journal.status(restore).unwrap().unwrap();
    assert_eq!(blocked.phase, Phase::Restoring);
    assert_eq!(blocked.blocker, Some(DeployError::TargetUnavailable));

    let status = recover_pending(&mut journal, &mut effects, &root(1), &root(2))
        .unwrap()
        .unwrap();
    assert_eq!(status.phase, Phase::Completed);
    assert_eq!(status.blocker, None);
    let state = journal.root_state(&root(2)).unwrap();
    assert_eq!((state.pending, state.active), (None, None));

    assert!(
        recover_pending(&mut journal, &mut effects, &root(1), &root(2))
            .unwrap()
            .is_none()
    );

    // The lease is genuinely released, not merely reported terminal.
    let later = empty_plan(&mut journal);
    assert!(journal.claim(Claim::Deploy(later)).is_ok());
}
