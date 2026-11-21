# Marduk Working Core Implementation Plan

> **For agentic workers:** Use `subagent-driven-development` only after the [global plan](2026-11-20-full-parity.md) and the assigned packet are approved. `- [ ]` steps are future actions; no command here has been run against product code.

**Goal:** Import two local ZIP mods, resolve one loose-file conflict, publish the winner, and restore the original safely on Linux and Windows.

**Architecture:** Deterministic profile and file resolution feeds a checked, journaled physical-copy method. SQLite stores logical state; OS adapters own durable effects. Minimal CLI and Tauri desktop use the same operations.

**Tech Stack:** Rust, Cargo, SQLite, Tauri 2, proposed React/TypeScript/Vite. Preview with synthetic Skyrim SE layout and no proprietary files.

**Spec:** [Preparation design](../specs/2026-11-20-m1-handoff-design.md), [architecture](../../architecture.md), [deployment research](../../research/deployment.md).

## Global Constraints

- One top-level packet per orchestrator session; list prerequisites in the ledger and verify before dispatch. No real game directories until G-RECOVERY/G-PATHS are closed for the exact configuration.
- Cover regular files under `Data` with existing destination parents only. The game-root marker files are inert fixtures. No target directory creation, redeploy without Restore, plugin load order, loader, Wine writes, downloaded content, private view or elevated privileges.
- First [filesystem proof targets](../../research/filesystem-support.md) are local Linux ext4 and Btrfs, and native Windows NTFS (x86-64). Copying from another library volume or Btrfs subvolume is not cross-domain publication: stage and replace within the target's rename domain.
- Commands below assume the named Cargo workspace from M1-A00 and the crate binaries added by their owning packets. Record target, OS, artifact digest and exact observed output; “not run” is not a pass.

## Review Focus

- Publisher wrote the file but its ACK was lost — reconcile actual names and bytes, not a boolean. M1-R02.
- Backup exists as a pathname but its data was not durable — no overwrite can precede the persistence barrier. M1-R02/M1-R04.
- A newly inserted junction, drive swap or colliding path invalidates a plan — no writes outside the captured root. M1-R01.
- Refused replace or corrupt archive cannot override the active mod and profile state. M1-C02.
- Desktop goes away or clicks twice mid-operation — clients can rediscover status and do not launch a duplicate write. M1-U01.

## Fixtures and shared interfaces

All M1 packets use `tests/fixtures/m1/README.md` fixture recipes. Create an isolated game tree with inert `SkyrimSELauncher.exe` and `SkyrimSE.exe` markers, `Data/textures/winner.dds` containing exact bytes `base\n`, and `Data/untouched.txt` containing `keep\n`. Mod A ZIP contains `textures/winner.dds = a\n` and `textures/a-only.dds = only-a\n`; mod B ZIP contains `textures/winner.dds = b\n`. Archives contain synthetic bytes only. Enabled `[A,B]` low-to-high resolves `b\n`; `[B,A]` resolves `a\n`. Restore must recover `base\n`, leave `keep\n`, remove `a-only.dds` if it still belongs to the managed deployment, and leave the staged mod payloads unchanged. An independent probe opens the resolved game path; do not pretend inert executables are a real game test.

The [C01 worker contract](../../contracts/m1-file-resolution.md) owns exact logical paths, pure resolver interfaces, and tests. R01 owns native placement and verification. Keep OS handles and native error objects out of public core types. At the operation seam, use typed `PlanId`, `RootIdentity`, `OperationId`, and `Revision`. A commit consumes an exact reviewed plan under the relevant locks, and status is queryable by operation ID. Stable errors include `StalePlan`, `Busy`, `TargetUnavailable`, `InvalidPath`, and `RecoveryConflict`; the shared operation packets own exact enum/type definitions and propagation.

## Session packets

Run the remaining stages together under [M1-CORE](2026-11-21-m1-complete.md). The sections below retain original ownership and proof dependencies; they are not separate launches for this run.

### M1-A00 — Baseline, workspace and native CI

**Status:** `verified` at `b54af92528166a55612ed73258783e4e35b9860a`. The [approved brief](2026-11-21-m1-a00-baseline.md) used rolling Rust `stable`, locked dependencies and one empty `marduk-core` crate. [Hosted CI](https://github.com/myyra0/Marduk/actions/runs/44906174341) passed format, locked build and locked tests on both native hosts. The baseline proves CI runs, not product behavior or native recovery. Each later packet owns its crate, and M1-U01 owns the desktop scaffold and checks.

### M1-C01 — Workspace, shared domain and resolver

**Status:** accepted at `022ef3a9866cb828c63dad5dff3d121e6d531fbc` after the scoped correction and Sol re-review, with [green hosted CI](https://github.com/myyra0/Marduk/actions/runs/44909371190). The [execution packet](2026-11-21-m1-c01-resolver.md) covers logical manifests only. Native filesystem placement belongs to M1-R01.

### M1-R01 — Root identity, portable relative paths and safe publication seam

**Predecessor:** M1-C01 stable IDs; named G-PATHS policy for this slice. Separate Linux and Windows worker writes; Sol reviews shared contract and both implementations.

**Owns:** `crates/core/src/root_identity.rs` (portable proof token only) and `crates/platform/{src/lib.rs,src/linux.rs,src/windows.rs,tests/root_contract.rs}`. Consumes C01's `targets.rs`; owns neither logical resolution nor journal/storage schema.

- [ ] **Planner:** Use the [native root contract](../../contracts/m1-root-verification.md) for authority, bounded lookup and five acceptance cases. Consume C01's [portable path policy](../../contracts/m1-file-resolution.md#required-behavior). Do not generalize M1's restriction to final path semantics; archive admission and actual filesystem admission are separate checks.
- [ ] **Workers:** Make platform own opening and proving root identity, bounded subpath operations and outcomes across absent/present/replaced/share-denied states. Reject unsupported configs before effects. Return typed failure plus observable state from any partial native replace when possible; do not claim generic `rename`/`ReplaceFileW` alone is durable.
- [ ] **Gate:** `cargo test -p marduk-platform --test root_contract` on real ext4, Btrfs and native NTFS; test actual junction/symlink with an outside sentinel and verify it remains `untouched\n`; check Btrfs subvolume substitution and report labels and skips. This packet does not close G-RECOVERY.

### M1-C02 — Local library, ZIP admission and metadata

**Predecessor:** M1-C01 for logical paths; local private-directory proof from M1-R01/R03 before claiming durable library publication. Pure ZIP cases are independent of the native proof. Work on independent ZIP and SQL files in parallel only after agreeing the stage/publication seam.

**Owns:** `crates/archive/{src/lib.rs,src/zip.rs,tests/admission.rs}`, `crates/storage/{src/lib.rs,src/library.rs,migrations/0001_library.sql,tests/library_contract.rs}`. SQL operation journal belongs to M1-R02.

- [ ] **Planner:** Use the [ZIP and library contract](../../contracts/m1-zip-library.md) to freeze dependency set, interface, record/expansion bounds and fixture witnesses. Pin exact duplicate, case alias, limits, invalid payload, interrupted/stale Apply and replay tests. Stop if metadata allocation is not bounded before parsing or release dependencies cannot be verified.
- [ ] **Workers:** Stage in a private allocated revision directory; publish by committing the SQLite catalog and membership reference only after durable staging. No extraction into an active revision or into the game. Orphaned staging must be recoverable/collectible without losing a referenced version.
- [ ] **Gate:** `cargo test -p marduk-archive --test admission` and `cargo test -p marduk-storage --test library_contract`; run the exact handoff fixture on both platforms. Verify negative tests also check no outside or active-state change.

### M1-R03 — Native publication proof and contract freeze

**Predecessor:** M1-R01. **Owns:** disposable proof under `tests/platform/m1/native-publication/`, reviewed table of native outcomes and accepted interface; no game startup, new production dependencies, or real-user files. Proof implementation requires separate approval. Use the [native qualification runbook](../../contracts/m1-native-proof.md) for candidate effects, exact witnesses and exit evidence.

- [ ] Linux candidate: stage on target filesystem, require ordered file and directory persistence, and evaluate an atomic name exchange that retains the displaced file for existing targets; use no-replace publication for absent targets. Record expected OS error/outcomes and no-overwrite behavior for an unexpected create.
- [ ] Windows candidate: stage on local NTFS, test a native replace/backup route with compatible sharing and flush semantics. Test documented partial outcomes, pre-existing read-only or sharing-denied files, required attributes/ACLs, and file vs namespace durability. Do not rely on `REPLACEFILE_WRITE_THROUGH`. Keep the actually displaced file if it differs from the approved pre-image.
- [ ] Qualify durable file and directory publication for a private library revision and its SQLite reference as a separate witness from replacing live game files. Demonstrate a guarded detachment for restore and return exact old/new/unknown outcomes; a test of replacing a regular file does not cover creation or removal.
- [ ] If a primitive cannot preserve or refuse displaced third-party bytes, or the claimed durability cannot be justified, return evidence to G-RECOVERY before writing production adapters. A proven subset may be proposed, not quietly put into production.

### M1-R02 — Operation plan, copy deployment and restart reconciliation

**Predecessor:** M1-R03 and M1-C02. **Blocked for execution** until the concrete durability/error-state contract is signed off; initial docs can be reviewed independently. No real install writes.

**Owns:** `crates/core/src/{deployment,recovery}.rs`, `crates/platform/src/physical_copy.rs`, `crates/storage/{src/operation_journal.rs,migrations/0002_operations.sql}`, `crates/core/tests/recovery.rs`. Planner owns the acceptance fixture and crash assertions; implementer can add tests without changing that oracle.

- [ ] **Planner:** Use the [physical-copy contract](../../contracts/m1-recovery.md) for typed operations, pre-image/durability barriers and late-ACK handling. Freeze the native outcomes against R03 proof. Carry the original backup vs step pre-image distinction into M2 redeploy.
- [ ] **Planner:** Supply the nine named recovery witnesses in that contract, including wrong volume, neither pathname nor identical bytes proving ownership, preserved external edits and replay after a committed response was lost. Tie actual child-process and VM cuts to the [recovery oracle](../../research/recovery-oracle.md).
- [ ] **Workers:** Implement immutable plan, cross-client root lock, journaled ordered file intents and typed outcomes. A restore must work without current profile metadata agreeing with originally deployed state. Validate disk-full, share-denied, missing original/backup and cancellation paths; never claim success or choose another volume/root to bypass them.
- [ ] **Gate:** `cargo test -p marduk-core --test recovery` and adapter/storage fault tests. The simulated persistence schedule must distinguish acknowledged and unacknowledged writes; it is not the OS proof.

### M1-U01 — Minimal CLI and desktop vertical slice

**Predecessor:** M1-C01/C02, testable M1-R02 operation contract; real install actions wait for proof. UI may test against a fixture/fake before the physical adapter is ready.

**Owns:** CLI crate scaffold, `crates/cli/{src/main.rs,tests/workflow.rs}`, desktop scaffold and lockfile under `app/`, `app/src/{features/workspace,domain,transport}`, `app/src-tauri/{src,capabilities,build.rs}`, fixture browser test and desktop CI steps on the M1-A00 matrix. Constrain ownership to the shared transport schemas, explicit commands and one screen; grow into the [M2 workspace](../../contracts/m2-workspace.md), not into a speculative design system.

- [ ] **Planner:** Use the [shared client contract](../../contracts/m1-clients.md) for commands, authority, state and acceptance witnesses. Freeze concrete DTO schemas, frontend stack/toolchain and browser runner before dispatch. Before `Apply`, show the target root, winner and original-backup status from the reviewed plan, not a second scan.
- [ ] **Workers:** Shell installs local assets only; restrict exposed commands. Execute the same core operations via CLI and desktop. Transport sequence reconnect obtains authoritative operation status; duplicate/stale submission must not cause another write. Keyboard reorder changes actual priority only when user chooses to reorder.
- [ ] **Gate:** `cargo test -p marduk-cli --test workflow`, approved frontend test command, then Linux/Windows packaged smoke with synthetic files. Drive import/reorder/deploy/restore and an interrupted/recovered display. Exact frontend runner and package paths are freeze requirements, not yet verified.

### M1-R04 — Recovery, storage and game smoke checkpoint

**Predecessor:** M1-R01/R02/U01. **Owns:** first proof harness and runbook under `tests/platform/m1/`, not production logic. Sol owns acceptance tests and final judgment.

- [ ] Run forced child-process termination at each meaningful prepare/publish/finalize/restore cut on each named OS/filesystem. Witness uses a real process and file comparisons; no mocked `exit`.
- [ ] Run the approved persisted vs lost/reordered-write fault model without assuming SQLite and game files are atomic together. Validate power interruption with abrupt VM power-off under recorded cache settings. Distinguish VM from physical proof, and never pass an untested guarantee. Unexpected barriers or native error outcomes go back to G-RECOVERY before broader adoption.
- [ ] Verify package prerequisites and a separately authorized real Skyrim SE loose-asset smoke on each supported runtime before marking G-SLICE complete. Record unsupported environment as `blocked`; don't broaden test effects to real user saves/config.
- [ ] C1 record links OS, filesystem, binary revision, test set, expected vs actual, safe reproduction in a fixture, and any remaining evidence. Not a feature or scope approval for later milestones.
