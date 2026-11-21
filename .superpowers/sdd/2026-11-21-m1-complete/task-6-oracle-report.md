# Task 6 recovery oracle freeze

Authority: architecture freeze §E/F, `docs/contracts/m1-recovery.md`, `docs/research/recovery-oracle.md`, ADR 0002. Tests only; no permission to enable native effects.

## Callable API

Types and fields are §E/F. Deploy/operation/generation IDs, status, plan, intent, bound target, source, phase, kind, pre-state and errors live in `marduk_core::deployment`. Journal, effect and evidence types live in `marduk_core::recovery`. In the signatures below `J: Journal`, `P: DeploymentInputs`, and `R<T> = Result<T, DeployError>`.

```rust
// marduk_core::deployment
pub fn preview_deployment<J: Journal, P: DeploymentInputs>(
    journal: &mut J, port: &mut P, profile: &ProfileSnapshot, inputs: &ResolvedInputs,
    installation: &RootIdentity, data: &RootIdentity,
) -> R<DeploymentPlan>;
pub fn commit_deployment<J: Journal, P: DeploymentInputs>(
    journal: &mut J, port: &mut P, id: PlanId, cancelled: impl FnMut() -> bool,
) -> R<OperationStatus>;
pub fn restore<J: Journal, P: DeploymentInputs>(
    journal: &mut J, port: &mut P, installation: &RootIdentity, data: &RootIdentity,
) -> R<OperationStatus>;
pub fn operation_status<J: Journal>(journal: &mut J, id: OperationId) -> R<OperationStatus>;

// marduk_core::recovery
pub fn recover_pending<J: Journal, P: DeploymentInputs>(
    journal: &mut J, port: &mut P, installation: &RootIdentity, data: &RootIdentity,
) -> R<Option<OperationStatus>>;
```

§F did not give these service signatures. **Required API additions/clarifications:**

- `deployment::DeploymentInputs: CopyEffects` supplies the authority/input seam. Associated types `RootGuard` and `LibraryGuard` own their acquired leases (`()` is valid for a fake). Methods, all `&mut self`: `try_root_gate() -> R<Self::RootGuard>`, `try_library_gate() -> R<Self::LibraryGuard>`, `verify_root(&RootIdentity, &RootIdentity) -> R<()>`, `resolve_target(&TargetFile) -> Result<BoundTarget, RootError>`, `profile_revision(ProfileId) -> R<ProfileRevision>`, `source(InstalledRevisionId, &RelativePath) -> R<SourceRef>` (verify immutable source/digest). These are trusted adapter calls, not IPC. Root gate precedes library gate; no gate or input reads for an already-recorded claim.
- Keep the §F `recovery::Journal` methods as written. Add `reopen(&mut self) -> R<()>`: discard the suspect connection before reading the operation/claim after ambiguous SQL IO, under the same root gate. Never call `finish` or a public effect a second time to resolve its ACK. If reopen cannot prove a terminal commit, do not report success.
- Keep the §E `recovery::CopyEffects` methods as written. Add `validate_backup(&mut self, at: BackupRef, expected: &Fingerprint) -> R<()>` to verify durable pre-images without creating a stage or touching the target. Compare digest, length and promised metadata to the recorded public pre-image; separately check the snapshot's own identity and owned slot. Missing/corrupt evidence is `RecoveryConflict`; root loss is `TargetUnavailable`. `StepOutput::Original(ref)` denotes a checked backup source: Restore names the generation's original; undo names the forward step pre-image. The original and step pre-image claims remain distinct.
- Checked `PlanId`, `OperationId`, `GenerationId`: `try_from_u64(u64) -> R<Self>`, `get(self) -> u64`; `Copy`, `Ord`, `Debug`. `BackupRef::from_record(OperationId, u32, StepDirection, BackupSlot) -> Self`, `StageRef::from_record(OperationId, u32, StepDirection) -> Self`; `BackupSlot::{Original, Step, Displaced}`. Typed construction takes only an already-checked ID/ordinal/direction/slot, never a path; decode still verifies the ref against its owning record. Refs and domain enums used for comparison are `Copy + Eq + Debug` where fieldless; record snapshots used by the fake are `Clone`.
- `marduk_storage::operation_journal::OperationJournal: Journal`: `open(app_data: &Path) -> R<Self>` opens/migrates the existing `catalog.sqlite3`; `select_verified_installation(&mut self, locator: &Path, installation: &RootIdentity, data: &RootIdentity) -> R<()>` persists the Setup pair, enforcing the root gate/pending/active rules. The synthetic locator is storage data, not permission to open or write a game directory.

`recover_pending` returns `Ok(None)` when there is no pending operation and `Ok(Some(status))` with a persisted blocker for a tracked operation it could not resolve. Restore without a generation returns `Err(NoActiveDeployment)` without allocating an operation. An interrupted Restore reports its source generation until completion; completion clears the active/result generation, not the historical source FK. An ambiguous platform barrier returns `Err(Io)` with the pending operation still inspectable and not finalized. Cancellation is sampled before public `Applying` and returns durable `Cancelled`, not a lost reply.

## Witnesses

Paths are relative to Data. Golden set: `textures/winner.dds=base\n`, `untouched.txt=keep\n`, `outside-plan.txt=user\n`; Deploy creates `textures/a-only.dds=only-a\n`, then replaces the winner with `b\n` (immutable input `a\n` loses). Restore reverses those operations. Every branch checks sentinels and source payloads, active generation independently of terminal status, and attempted public mutations, including retries.

| Witness | Cut and independently expected result |
|---|---|
| `cut_after_record_before_publish` | Durable `Applying`, first row `Attempting`, second `Ready`, zero public mutations. Boot → `RolledBack`, winner `base\n`, created file absent, no generation, still zero calls on repeat. |
| `cut_after_publish_before_ack` | Both names durable, winner's SQL `Applied` write lost; persisted rows `Applied`/`Attempting`. Boot → reverse replace/detach; both `Undone`, `RolledBack`, original bytes/metadata, no generation; four total calls. Stale/unavailable replay still reports same rolled-back claim. |
| `failed_final_barrier_no_success` | Winner replacement is visible but final platform barrier returns the same ambiguous result in both histories. No finalize, `Applying`/`Io`, no generation. Boot only reverts the winner if the rename survived; both histories end `RolledBack` with base/absence and no generation (four or three calls), not by interpreting error text. |
| `committed_before_reply_does_not_reapply` | Durable finalize loses SQL ACK; service reopens and inspects, one finalize. Boot and replay stale/unavailable ID → same `Completed`/generation, staged bytes installed, two total effects. |
| `restore_twice_is_noop` | Restore publishes durable `base\n` with no step ACK, while created file is still managed and generation remains active. Boot confirms winner without a second replace, detaches created file, completes/clears active; next Restore `NoActiveDeployment`, four total calls. Original `base\n`, current step snapshot `b\n`, and displaced `b\n` have distinct recovery claims. |
| `restart_preserves_external_edit_and_backup` | External `edit\n` replaces winner after cut. `RollingBack`/`RecoveryConflict`, preserves exact external object and recoverable original, retains pending, no active generation; two calls total, even on retry. |
| `wrong_volume_waits` | Pending Restore encounters same-looking location on wrong volume: `Restoring`/`TargetUnavailable`, original evidence and source generation retained, no target reads or writes on wrong volume. Rebind exact pair → complete Restore, four calls total. |
| `unknown_external_file_is_not_removed` | External created-path replacement has identical digest and metadata but different identity. Roll back winner only, leave exact external object; `RollingBack`/`RecoveryConflict`, pending and no active generation, three calls total even after retry. |
| `missing_parent_is_refused_before_effects` | Later canonical target has missing parent at preview or after eligible preview; `InvalidPath`, no private staging, zero public writes and no pending/active deployment. |

Supporting cases: a changed original refuses the entire plan before private/public effects; pre-public cancellation is terminal and replayable; an unowned second plan cannot enter a pending root; missing and corrupt step snapshots both block rollback even with the original still recoverable. The core fake boots two independent durable images. The SQLite tests migrate seeded v1 rows, inspect separately opened connections, reject stale CAS and premature finalize, and discard the real committed ACK before reconnecting to replay via the service with all effect/input calls forbidden. The fake additionally verifies automatic reopen when `finish` returns `Io` after durable SQL commit. Neither model qualifies native effects or VM durability.

Verification: both targeted `cargo test --locked` invocations and `cargo test --workspace --locked` reached **deferred red** (`E0432` for the missing core `deployment`/`recovery` modules and storage `operation_journal`); no tests executed. `cargo fmt --all -- --check` and `cargo build --workspace --locked` passed. SQLite call shapes checked against [rusqlite upstream documentation](https://github.com/rusqlite/rusqlite) via Context7; no external recovery design supersedes §E/F.
