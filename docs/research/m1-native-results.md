# M1-R03 native results

Runtime evidence from 2026-11-22 (IST), code commit `29e587d`, base `ebd9e07`. Task 6 native-adapter rows were re-tested at `101b4f7` (recovery fix `7feb64b`). `not run` is never a pass. No power-cut or VM evidence exists.

Close-out 2026-11-22: the Task 5 gate blocked native wiring; a later Task 5 continuation connected private-library staging on the two Linux volume classes with runtime evidence (Btrfs and tmpfs), and the Windows staging adapter is implemented and witnessed on NTFS while its product gate stays closed; see [Qualified library staging](#qualified-library-staging-connected-2026-11-22). Native game-directory preview, Apply and Restore are connected in the workspace proof lane on the two Linux volume classes (Task 6), but client `dispatch` still refuses them behind the closed C1 gate. `not run` rows stay `not run`.

Environment: Linux 7.3.5-1-cachyos, x86-64, rustc 1.100.0, uid 1000 with `CapEff: 0` (ordinary user, no elevated token). tmpfs scratch: `/tmp` `rw,noatime,inode64,huge=advise`, fsid `b6b575e7…`, mount view 74, no Btrfs tree. Btrfs scratch: `/home` on `/dev/nvme0n1p2[/@home]` `rw,noatime,compress=zstd:1,ssd,discard=async,space_cache=v2,subvolid=257,subvol=/@home`, fsid `b68b24bc…`, view 61, tree 257. All fixtures are synthetic files under a disposable parent; no game, external root or elevated operation was touched.

| Proof case | Linux Btrfs | Linux tmpfs | Linux ext4 | Hosted `windows-2025` NTFS | Power |
|---|---|---|---|---|---|
| `native_no_replace_honors_concurrent_create` | ran 2026-11-22, passed | ran 2026-11-22, passed | not run | not run | — |
| `native_replace_records_actually_displaced_file` | ran 2026-11-22, passed | ran 2026-11-22, passed | not run | not run | — |
| `native_rejects_root_and_parent_substitution` | ran 2026-11-22, passed | ran 2026-11-22, passed | not run | not run | — |
| `native_detach_preserves_unknown_file` | ran 2026-11-22, passed | ran 2026-11-22, passed | not run | not run | — |
| `native_error_does_not_prove_no_change` | ran 2026-11-22, passed | ran 2026-11-22, passed | not run | not run | — |
| `native_namespace_survives_claimed_barrier` | ran 2026-11-22, passed (process-only, new create only) | ran 2026-11-22, passed (process-only, new create only) | not run | not run | Task 8, not run |
| `library_catalog_never_names_missing_payload` | not run (staging wired on this volume; catalog witness not run) | not run (staging wired on this volume; catalog witness not run) | not run (unqualified; `StageUnavailable`) | not run (unqualified; `StageUnavailable`) | Task 8, not run |
| `btrfs_cross_subvolume_copy_does_not_use_cross_subvolume_rename` | not run (subvolume teardown EPERM; two-subvolume `EXDEV`/substitution not run) | N/A | N/A | N/A | — |

**Evidence scope:** The root/parent substitution row covers refusal when substitution was visible at the last live check. It does not demonstrate prevention inside the final check-to-syscall window.

## Per-row evidence

1. **No-replace create** — the child created `Data/textures/new.dds = edit\n` at the `BeforeNativeEffect` cut, before the port issued any rename. The port's recheck observed the parent identity/leaf and the now-present target and refused with `Conflict`; no native `renameat2` `EEXIST` was reached. Target retained `edit\n`; the private stage still held `mod\n`; sentinels unchanged.
2. **Replace with actual displacement** — child moved `winner.dds = base\n` to `old`, wrote `edit\n` at the public name and recorded its independent file identity. After the checkpoint the port observed an identity different from the frozen pre-image and refused cleanly: outcome `Conflict`, `edit\n` remained public, stage `mod\n` intact, and both independent recovery copies (`original` and `step`) held `base\n` at separate operation-owned refs. The harness accepts this refusal branch or the exact-retention branch; the refusal branch was the observed one.
3. **Root and parent substitution** — the child moved `Data/textures` outside `Data` and put a symlink in its place. Re-resolution through the live binding failed (`RESOLVE_NO_SYMLINKS`/beneath), so the port returned `Unavailable(OutsideBoundary|TargetUnavailable)` or `Conflict` before any rename. `moved-parent/winner.dds` retained `base\n`, the stage retained `mod\n`, and no outside sentinel changed.
4. **Checked detach** — same swap race with no staged output: outcome `Conflict`, the child's `edit\n` remained at the public name, no retained slot was created, and nothing was deleted or settled as a removal.
5. **Error is not "no change"** — injection returned `Fault::LoseAcknowledgement` after a successful exchange. The port reobserved and returned `Ambiguous` with the new target `mod\n`, the canonical retained slot holding `base\n`, and the logical stage absent. Genuine missing-target and read-only-metadata (`user.r03` xattr) refusals were exercised separately and were not reported as success. No claim is made for Win32 1175/1176/1177.
6. **Namespace barrier** — process-only cuts for a **new create only**. Before publish the target was absent; after publication and before the barrier the child process saw the new name; after the claimed Linux barrier a reopened verifier saw the same object identity and bytes. The replace, detach and Restore barriers are `not run`. No power-loss or VM-restart claim is made.
7. **Catalog never names a missing payload** — `not run`. Private library staging is now wired on Linux Btrfs and Linux tmpfs (see below), and on an unqualified volume `Catalog::open` keeps the `Unavailable` stage; the interrupted-stream/prepared/committed cut witness itself has not been re-run in the current tree, so no pass is claimed.
8. **Btrfs cross-subvolume** — `not run`. `btrfs subvolume create` succeeded and `btrfs subvolume delete` returned `EPERM` ("Operation not permitted"), which the runbook treats as not run. The scratch path was retained; no shim or other filesystem was substituted. The matching-inode substitute-subvolume assertion and the `EXDEV`/cross-subvolume copy check belong to this unrun two-subvolume case; they were not exercised.

## Commands

- `python tests/platform/m1/native-publication/run.py /tmp/opencode/r03-run` (tmpfs): exit 0; 8 passed, 1 ignored; library case reported `R03 NOT RUN: catalog native receipt unavailable on this volume`; subvolume case reported not run.
- `python tests/platform/m1/native-publication/run.py /home/parth/.cache/r03-run3` (Btrfs): exit 0; 8 passed, 1 ignored; the library case then ran the real catalog cuts against volume-selected native staging (Btrfs only at that revision). That observation is superseded by the disconnected revision and was not re-run after the qualified reconnection below; subvolume case reported teardown EPERM.
- `cargo test --locked -p marduk-platform --test native_publication`: compiles and passes; without `MARDUK_R03_SCRATCH` every runtime case prints `R03 NOT RUN`.
- `cargo test --locked -p marduk-storage --test library_contract` (`TMPDIR=/tmp`): 5 passed; at that revision the production stage was `Unavailable` on every volume (now volume-selected, see below).
- `cargo check --locked --target x86_64-pc-windows-msvc -p marduk-platform --tests`: passed; this is compile-only, not NTFS runtime.
- `cargo fmt --all -- --check`, `cargo build --workspace --locked`, `cargo test --workspace --locked`: passed.

The frozen `run.py` builds its probe with only `.rlib` artifact directories. In this workspace the `zeroize_derive` proc-macro artifact lives in a separate build directory, so the probe first failed with `E0463: can't find crate for marduk_storage`. Symlinking the hashed proc-macro `.so` into `target/debug` (already a search path) let the frozen script complete unchanged. The harness assertions themselves were not modified; no assertion failed.

## Residual closure (2026-11-22)

Adds `crates/platform/tests/native_proof_evidence.rs` detach witnesses, `library_stage.rs` error classification and a private `revisions/` mode. Frozen harness and oracle unchanged.

### Library staging (disconnected 2026-11-22, superseded below)

The Task 5 gate blocked native library wiring at that revision: `Catalog::open` used `StageAdapter::Unavailable` on every volume. The Btrfs library witness was therefore `not run` there, and the earlier Btrfs-only observation (selected from the catalog root's filesystem magic, `crates/platform/src/linux.rs:904` at the time) did not qualify the disconnected code. The frozen `library_contract` reported the same 5 passes under a Btrfs `TMPDIR` because staging was disconnected everywhere. This paragraph is history; the next section is the current state.

### Qualified library staging (connected 2026-11-22)

Tested at commit `a2250e9`, base `252ce26`.
Continuation of Task 5 after the 2026-11-22 ruling. `Catalog::open` now selects native staging only when `marduk_platform::library_volume_qualified` reports a volume class with recorded runtime evidence: **Linux Btrfs** and **Linux tmpfs**. The predicate reads the observed private library root with the crate's existing `fstatfs` path (`crates/platform/src/linux.rs`), not a `statfs` type string. The catalog refuses with `StageUnavailable`:

- Linux ext4 and every other unrecognised magic, including `FUSE` (ntfs-3g) and `ntfs3`.
- Windows product availability: the staging adapter is implemented and witnessed on native NTFS (below), but `library_volume_qualified` and `effects_volume_qualified` stay `false`, so catalog imports and deploys remain disconnected.
- A qualified volume whose private boundary will not bind: a redirected root or `revisions/`, an owner or mount mismatch, or a two-subvolume/`EXDEV` layout with no evidence. `NativeStage::open` refusal is final; it is never retried through a weaker adapter.

Wired: private staging (private archive snapshot, admitted member set, hashed payload under `revisions/<id>/`, `sync_revisions` before the receipt) and catalog publication of a prepared import into the library (SQLite membership only, after native re-observation). Separate from library staging, Task 6 connected the native copy adapter for game-directory preview, Apply and Restore in the workspace `proof_*` lane on qualified volumes; client `dispatch` refuses `deploy_preview`, `deploy_apply` and `restore` with `UnsupportedTarget`/`Configuration`. No client-visible native write is unlocked, and no power-cut claim exists.

| Suite / test | Required volume | Exact run condition | Observed |
|---|---|---|---|
| `crates/storage/tests/qualified_stage.rs::qualified_volume_stages_the_payload_through_the_production_catalog` | tmpfs or Btrfs | `TMPDIR=/tmp cargo test --locked -p marduk-storage --test qualified_stage`; also run with `TMPDIR=/var/tmp` | passed on tmpfs and Btrfs 2026-11-22; asserts the volume class instead of skipping; payload read back at `revisions/<id>/payload/textures/synthetic.dds`, revision dir `0700`, `reconcile` re-observes the receipt |
| `crates/storage/src/library/stage_tests.rs::refused_volume_configuration_keeps_the_stage_unavailable` | any | `TMPDIR=/tmp cargo test --locked -p marduk-storage` | passed; `StageUnavailable`, no import row, no `revisions/` entry |
| `crates/platform/src/linux_root_tests.rs` predicate tests (5) | any | `cargo test --locked -p marduk-platform --lib` | accept Btrfs/tmpfs; refuse ext4, FUSE/NTFS3 and unknown magic |
| `crates/storage/tests/library_contract.rs` (5 tests) | any | `TMPDIR=/tmp cargo test --locked -p marduk-storage --test library_contract`; also with `TMPDIR=/var/tmp` | 5 passed on both; the volume-dependent test is now `production_stage_follows_the_qualified_volume` (refusal on unqualified, staging plus re-observation on qualified) |
| `crates/storage/tests/production_refusal.rs` | any | `TMPDIR=/tmp cargo test --locked -p marduk-storage --test production_refusal` | 1 passed; an unprovable seeded stage row is never published on either adapter |
| `crates/platform/tests/library_boundary.rs` (10 witnesses) | Btrfs | `MARDUK_M1_BTRFS_SCRATCH=/var/tmp cargo test -p marduk-platform --test library_boundary -- --ignored --test-threads=1` | 10 passed; without `--ignored` 10 ignored, none silently passing |
| `crates/storage/src/library/contract_tests.rs` (11 unit tests, frozen oracle) | any | `Catalog::open_for_test` forces the fixture stage | 11 passed under `TMPDIR=/tmp` |
| `crates/workspace/tests/workflow.rs::import_preview_and_apply_round_trip_on_a_qualified_volume` | tmpfs or Btrfs | `TMPDIR=/tmp cargo test --locked -p marduk-workspace` | passed; preview stages, apply publishes the recorded revision once, replay returns the same snapshot |
| `library_catalog_never_names_missing_payload` (`tests/platform/m1/native-publication/library.rs:134`) | tmpfs or Btrfs | `python tests/platform/m1/native-publication/run.py <scratch>` | `not run` in this continuation; the table row above stays `not run` |

The runtime evidence behind the accepted magic set is the tmpfs/Btrfs effect table at the top of this document plus the catalog staging witnesses above. No ext4 or native NTFS catalog run exists; those cells stay `not run`.

Windows product availability stays closed: `library_volume_qualified` and `effects_volume_qualified` are `false`, and deploy-level NTFS rows remain `not run`. The private staging adapter itself is implemented and was re-run on NTFS after the 2026-11-22 review fixes:

- Tree: `7bcd2c1` plus the staging review fix. VM: Windows 10 `DESKTOP-4NP2KM5`, `rustc 1.100.0`. Scratch volume `C:` reported `File System Name : NTFS` (`fsutil fsinfo volumeinfo C:`).
- `cargo test -p marduk-platform --test library_staging_witnesses -- --nocapture` → `running 8 tests`, `test result: ok. 8 passed; 0 failed`, including `reobserve_refuses_an_added_payload_file`.
- Privacy was asserted by an independent `GetKernelObjectSecurity` DACL scan (not the adapter's `private_dacl`) for `revisions/<id>`, `payload/`, `payload/textures/` and `payload/textures/synthetic.dds`: protected DACL, owner equal to the token's default owner, owner-only ACEs, the object ACE plus the inherit-only descendant ACE on every directory, and exactly one non-inherit-only ACE on the file. `icacls` showed `BUILTIN\Administrators:(F)` plus `BUILTIN\Administrators:(OI)(CI)(IO)(F)` on the revision and `BUILTIN\Administrators:(F)` on the file; the SSH token is elevated, so Administrators is the token owner.
- The redirect witnesses took the `symlink_dir` branch (logged by `link_directory`), not the `mklink /J` fallback, under the elevated SSH token; the junction branch is unexercised. That is an evidence gap, not a pass.
- `cargo test --workspace --locked` on the VM → exit 0, no failures. Linux `TMPDIR=/tmp cargo test --workspace --locked`, `cargo fmt --all -- --check`, `cargo build --workspace` and the MSVC-target `cargo check -p marduk-platform --all-targets --target x86_64-pc-windows-msvc` also passed.

Deploy-level NTFS rows remain `not run`; only the private staging adapter is evidenced here.

### Detach and classification witnesses

`MARDUK_R03_SCRATCH` on Btrfs and tmpfs, `--test-threads=1`: `cargo test --locked -p marduk-platform --test native_proof_evidence` → 7 passed on each volume.

- `detach_rejects_wrong_retain_slot_without_touching_target`: `BackupSlot::Original` as the detach retain returns `Conflict`, the target keeps `base\n`, and the operation-owned `displaced` slot stays absent.
- `detach_native_error_with_missing_target_and_occupied_slot_is_ambiguous`: the target disappears and the displaced slot is written at `NativeRename`; `renameat2` fails, reobservation reports target `Absent` with a non-empty retained slot, and the outcome is `Ambiguous`, never `Observed`.
- `library_stage.rs:210` now maps a refused/exhausted `create_new` (permission, ENOSPC, I/O) to `StageUnavailable` and only `AlreadyExists` to `StageChanged`.
- `library_stage.rs:274` maps `ArchiveError::Io` from `admit_zip` to `StageUnavailable`; every other admission error stays `StageChanged`. No new test drives either branch: both need an injected syscall failure, so only the classification code changed.
- `Catalog::open` creates `revisions/` with mode `0700` (`private_directory`); `revisions_directory_is_private_to_the_owner` asserts `0700` on create and after reopen.


### Handle-relative private staging (2026-11-22)

Base `8110c38`. `NativeStage` now keeps every archive and payload effect on the verified revision handle: parents are created with `mkdirat(0700)`, leaves with `openat2(O_CREAT|O_EXCL|O_NOFOLLOW)`, and hashing, listing, fsyncs and reclaim unlinks resolve beneath the handle with `RESOLVE_BENEATH|RESOLVE_NO_SYMLINKS|RESOLVE_NO_XDEV`. The revision name is removed through the bound `revisions/` handle. This closes the path-join window for bytes; it does not qualify publication.

- `MARDUK_M1_BTRFS_SCRATCH=/var/tmp cargo test -p marduk-platform --test library_boundary` — 8 witnesses ignored without `--ignored` (none pass silently); with `--ignored --test-threads=1`, 8 passed. (The suite has since grown to 10 witnesses; the qualified re-run records 10 passed.)
- New witnesses redirect a staged revision after its handle was opened and require `StageChanged`/`StageUnavailable` from `reobserve` and `reclaim` with the redirect target untouched and the original revision retained.
- `TMPDIR=/tmp cargo test -p marduk-platform`, `TMPDIR=/tmp cargo test -p marduk-storage`, `cargo fmt --all -- --check`, `cargo build --workspace --locked`, `TMPDIR=/tmp cargo test --workspace --locked` — passed.
- Windows compile check passed; the NTFS staging capability is implemented and witnessed at the adapter layer below, while product availability stays closed. `Catalog::open` connects native staging on qualified Linux volumes only (see above). Apply/Restore status is stated once in the Task 6 section below.

**Known limitation (accepted by the 2026-11-22 user ruling):** On Linux and Windows, a same-privilege process can substitute the parent or leaf between the final live-handle check and the syscall. The [R02 contract](../contracts/m1-recovery.md#native-seam-that-r03-must-qualify) requires post-effect reobservation to detect divergence, retention of actually displaced bytes, and refusal to retry automatically on divergence. It does not claim prevention. A syscall error is not evidence of no change.

The ruling changes the qualification criterion; it supplies no runtime evidence. Native staging is now wired only on Linux Btrfs and Linux tmpfs, and remains unqualified and unwired on ext4 and native NTFS. Game-directory publication through the native adapter is connected in the workspace proof lane on those two Linux volume classes; client Apply and Restore stay behind the closed C1 gate. Power-cut behaviour remains unqualified.

## Native copy adapter (Task 6, 2026-11-22)

Base `5fe1a02`. Adapter commit `b522b48`; primitive promotion `3f03ac8`;
Windows compile fix `8fd7f79`; restart-durability fix `7feb64b`; preview/manifest
alignment `101b4f7`. The checked effect primitives moved out of the hidden proof
harness into `marduk_platform::effects`; `native_proof` now calls the same code.
A production `marduk_storage::native_effects::NativeEffects` adapter implements
`DeploymentInputs` and `CopyEffects` against the journal's recorded locator and
the app-owned library root. It refuses an unqualified volume, reopens the
recorded locator, uses bound targets, retains actually displaced bytes, obtains
the root and library gates in order, and records `Applied` only after the
platform returns durability evidence.

The workspace reaches the adapter only through its `proof_*` methods. Client
`dispatch` and status remain typed unavailable because the C1 proof lane has not
run; the single decision point names this document. No real game directory was
written.

All rows below re-ran on 2026-11-22 at `101b4f7` on both qualified Linux volume
classes.

| Witness | Linux Btrfs | Linux tmpfs | Linux ext4 | Hosted `windows-2025` NTFS |
|---|---|---|---|---|
| `qualified_native_round_trip_retains_backups_and_restores_the_original_tree` (workspace: setup → import → order → preview → apply → restart → replay → restore) | ran 2026-11-22 at `101b4f7`, passed | ran 2026-11-22 at `101b4f7`, passed | not run | not run |
| `restart_after_apply_before_ack_rolls_back_and_never_reapplies` | ran 2026-11-22 at `101b4f7`, passed | ran 2026-11-22 at `101b4f7`, passed | not run | not run |
| `restart_after_record_undo_uses_the_sealed_stage_fingerprint` | ran 2026-11-22 at `101b4f7`, passed | ran 2026-11-22 at `101b4f7`, passed | not run | not run |
| `restart_mid_restore_continues_in_reverse_order` | ran 2026-11-22 at `101b4f7`, passed | ran 2026-11-22 at `101b4f7`, passed | not run | not run |
| `concurrent_edit_during_apply_conflicts_and_preserves_external_bytes` | ran 2026-11-22 at `101b4f7`, passed | ran 2026-11-22 at `101b4f7`, passed | not run | not run |
| `restore_time_external_edit_conflicts_and_preserves_external_bytes` | ran 2026-11-22 at `101b4f7`, passed | ran 2026-11-22 at `101b4f7`, passed | not run | not run |
| `recorded_locator_mismatch_is_a_typed_refusal` | ran 2026-11-22 at `101b4f7`, passed | ran 2026-11-22 at `101b4f7`, passed | not run | not run |
| `unqualified_installation_root_is_refused_before_any_write` | ran 2026-11-22 at `101b4f7`, passed (procfs installation) | ran 2026-11-22 at `101b4f7`, passed (procfs installation) | not run | not run |
| `unqualified_app_root_is_refused_before_any_write` | ran 2026-11-22 at `101b4f7`, passed (sysfs app root) | ran 2026-11-22 at `101b4f7`, passed (sysfs app root) | not run | not run |
| `unreadable_private_slot_is_unavailable_not_absent` | ran 2026-11-22 at `101b4f7`, passed | ran 2026-11-22 at `101b4f7`, passed | not run | not run |
| `prepared_private_artifacts_are_durable_and_unpublished` | ran 2026-11-22, passed | ran 2026-11-22, passed | not run | not run |
| `private_chain_is_synced_before_the_first_public_effect` | ran 2026-11-22, passed | ran 2026-11-22, passed | not run | not run |
| `sync_chain_stops_at_the_resolved_root_for_a_symlinked_library_spelling` | ran 2026-11-22, passed | ran 2026-11-22, passed | not run | not run |
| `unqualified_root_answers_a_claimed_plan_and_pending_restore` (workspace) | ran 2026-11-22, passed | ran 2026-11-22, passed | not run | not run |
| `clean_unqualified_root_answers_before_opening_a_native_port` (workspace) | ran 2026-11-22, passed | ran 2026-11-22, passed | not run | not run |
| `dispatch_stays_typed_unavailable_after_native_composition` (preview, apply and restore) | ran 2026-11-22, passed | ran 2026-11-22, passed | not run | not run |

The workspace round trip uses disposable synthetic roots only. It verifies the
deployed bytes, the retained `original` backups for replaced files, the absent
original slot for a created file, the committed generation after a restart, a
no-op recovery, a replay that reports the recorded operation, a reverse-order
restore, and the exact original file tree after restore. The restart cut applies
and barriers the winner, loses the step record, reopens the journal, rolls back
under recovery, and proves a second recovery does not reapply.

### Restart durability and evidence correction (`101b4f7`)

- Recovery no longer depends on the map that `prepare` built. Every
  `inspect_step` copies the sealed `staged` fingerprint from the journal-derived
  `PreparedStep` before `execute`/`execute_undo`, so a reopened adapter can
  classify and resume an exchange. Both restart witnesses above cut the port and
  reopen before recovery and assert real target bytes and the terminal phase.
- `artifact` maps only a `symlink_metadata` `NotFound` to `Absent`. A stat that
  succeeds followed by an open/read failure is `Unavailable`, so an unreadable
  slot cannot satisfy `matches_post` as if it were empty.
- `prepare` fsyncs the operation directory and its ancestors up to the app data
  root before returning. `prepare_persists_private_artifacts_before_publication`
  reopens the port after prepare, verifies the private evidence and the unchanged
  public tree, and resolves the prepared operation as `Cancelled`. This is a
  process-restart witness, not power-loss evidence for directory fsync ordering.
- The workspace consults `Journal::claimed` for Apply and `root_state`/`status`
  for Restore before opening the native port, so a claimed replay reports its
  recorded status and a clean root reports `NoActiveDeployment` instead of
  `UnsupportedTarget`.
- `resolved_inputs` inserts a manifest only for a published revision: an unknown
  revision is omitted (`MissingManifest` can fire) and a published revision with
  no rows is an empty manifest.
- Preview `winner`/`candidates` are member ids, matching the fixture and the TS
  DTO; a fixture drift witness asserts they are never installed revision ids.

### Primitive correction

While exercising the moved copy primitive, the adapter's metadata witness
failed on a freshly copied backup. `copy_file` passed the `futimens` array as
`[mtime, atime]`; POSIX expects `[atime, mtime]`. The primitive now preserves
the source modification time. This is a correctness fix in the promoted code,
not a change to any contract or oracle. `native_proof_evidence` was re-run on
both qualified volumes afterwards: 7 passed on each.

### Commands

- `TMPDIR=/tmp cargo test -p marduk-storage --test native_effects -- --ignored --test-threads=1` — 12 passed after the re-review fix.
- `TMPDIR=/var/tmp cargo test -p marduk-storage --test native_effects -- --ignored --test-threads=1` — 12 passed after the re-review fix.
- `TMPDIR=/tmp cargo test -p marduk-workspace --test native_deployment -- --ignored --test-threads=1` — 4 passed after the re-review fix.
- `TMPDIR=/var/tmp cargo test -p marduk-workspace --test native_deployment -- --ignored --test-threads=1` — 4 passed after the re-review fix.
- `TMPDIR=/tmp` and `TMPDIR=/var/tmp cargo test -p marduk-storage --test qualified_stage -- --ignored` — 1 passed on each.
- `MARDUK_R03_SCRATCH=/tmp` and `/var/tmp cargo test -p marduk-platform --test native_proof_evidence -- --test-threads=1` — 7 passed on each.
- `MARDUK_R03_SCRATCH=/tmp` and `/var/tmp cargo test -p marduk-platform --test native_publication -- --test-threads=1` — 8 passed, 1 ignored on each after the `futimens` correction.
- `TMPDIR=/tmp cargo test -p marduk-platform -p marduk-storage -p marduk-workspace -p marduk-cli` — passed at `101b4f7`.
- `cargo fmt --all -- --check`, `cargo build --workspace --locked`, `TMPDIR=/tmp cargo test --workspace --locked` — passed at `101b4f7`.
- `cargo check -p marduk-platform --tests --target x86_64-pc-windows-msvc` — passed at `101b4f7`; compile-only, not NTFS runtime evidence.

Linux ext4 and native NTFS remain `not run`. Windows compilation is not NTFS
runtime evidence; see [Windows native effects](#windows-native-effects-implemented-2026-11-22-runtime-not-run)
for the implemented surface and its unrun runtime rows. The final-window
substitution limitation in
[R02](../contracts/m1-recovery.md#native-seam-that-r03-must-qualify) is unchanged:
the adapter detects divergence after an effect and retains displaced bytes; it
does not claim prevention.

## Windows native effects (primitive runtime only; deploy-level not run)

The Windows arms of `crates/platform/src/effects.rs` implement the same
contract as the Linux arms. The six `effects_primitives` witnesses pass on the
hosted `windows-2025` job at `eb39be0` on real NTFS. That is primitive-level
evidence: no deploy-level NTFS row, end-to-end adapter run or power-cut evidence
exists yet; `not run` is never a pass, and qualification stays closed.

| Primitive | Contract implemented | Linux runtime | Hosted `windows-2025` NTFS runtime | Power |
|---|---|---|---|---|
| `effects::copy_file` | `CREATE_NEW` destination, data stream, attributes, creation/access/last-write times, owner/group/DACL, `FlushFileBuffers`; an occupied destination maps to `CopyError::DestinationExists` (conflict), a lock to `CopyError::Sharing` (retryable `Io`), and only unreproducible metadata to `Unsupported(Metadata)`; a source DACL that grants write (concrete bits, `GENERIC_WRITE`, `GENERIC_ALL`, or fail-closed `MAXIMUM_ALLOWED`) to a principal other than the current user or the SYSTEM/Administrators baseline is refused before it reaches the private area, including inherited grants, and every post-create failure removes the partial file | `effects_primitives::copy_preserves_bytes_and_last_write_time_and_refuses_existing`, `copy_refuses_concrete_file_write_data_in_source_dacl`, `copy_refuses_generic_write_in_source_dacl` and `copy_refuses_generic_all_in_source_dacl` passed 2026-11-22 (Linux) | not run | — |
| `effects::publish` | one `MoveFileExW` with flags `0` (no `MOVEFILE_REPLACE_EXISTING`, no `MOVEFILE_COPY_ALLOWED`); an existing target fails and is left intact | `effects_primitives::publish_refuses_an_existing_target_and_leaves_it_intact` passed 2026-11-22 | not run | — |
| `effects::replace` | exchange by two no-replace renames in production (target → `<operation>/displaced`, stage → target); a caller that retains at a different path adds a third rename from that slot; a cut after the first rename leaves the canonical `displaced` slot plus the staged object and resumes from that shape, which `evidence_for`, `prepare` and `sync_effect` inspect; the staged object's file id, creation time, attributes and DACL are preserved, so the public target fingerprints as the sealed staged artifact; a failure returns an error, never `Ok` | `effects_primitives::replace_retains_the_displaced_file_and_keeps_the_staged_object` and `interrupted_replace_keeps_the_canonical_displaced_slot_and_resumes` passed 2026-11-22 | not run | — |
| `effects::detach` | one no-replace rename into the retain slot; an occupied slot fails, unknown bytes are never replaced | `effects_primitives::detach_preserves_an_unknown_file` passed 2026-11-22 | not run | — |
| `effects::sync_directory` | `FlushFileBuffers` on a directory handle opened with `FILE_FLAG_BACKUP_SEMANTICS`; not a namespace or power-cut barrier | `effects_primitives::sync_directory_succeeds_on_a_real_directory` passed 2026-11-22 | not run | not claimed |
| `effects::volume_qualified` | `false` on Windows until deploy-level evidence exists. The NTFS probe (from `GetVolumeInformationByHandleW` on the opened root) is kept for the private staging lane; NTFS alone is not qualification | `effects_primitives::volume_qualification_reads_the_opened_root` passed 2026-11-22 (refusal branch) | not run | — |

Design note: `ReplaceFileW` is deliberately not used. It merges the replaced
file's creation time and DACL into the replacement, so the resulting public file
would not match the sealed staged `Fingerprint` (identity includes the creation
time; metadata includes the owner/group/DACL digest). The exchange renames the
staged object itself, which keeps every witnessed field. The target name is
absent between the two production moves, so this is not an atomic exchange;
every failed step returns an error and the caller must re-observe. A caller that
retains the displaced object at a different path performs one additional
no-replace rename. The intermediate is `<operation>/displaced`, not a name
derived beside the stage, so the adapter's leftover guard, evidence scan and
directory sync all see the displaced bytes after a cut.

The six file-level primitives passed hosted `windows-2025` at `eb39be0` on real
NTFS (the run that exposed and then verified the tunneling fix). That is
primitive-level evidence only: deploy-level NTFS rows remain `not run`, the
namespace barrier stays `Unavailable`, and `effects_volume_qualified` therefore
returns `false` on Windows. Native Apply stays disconnected behind
`native_effects_gate`.

Windows `native_namespace_survives_claimed_barrier` remains blocked: the
durability barrier still reports `UnsupportedTarget(Configuration)` because
Windows documents no ordinary-token namespace persistence barrier. Client
deploy/restore `dispatch` stays closed behind `native_effects_gate`.

Commands run for this row set:

- `cargo check -p marduk-platform --all-targets --target x86_64-pc-windows-msvc` — passed (compile only).
- `cargo test -p marduk-platform` (host Linux) — passed, including 6/6 in `tests/effects_primitives.rs`.
- `cargo fmt --all -- --check` and `cargo build --workspace --locked` — passed.


## Test-oracle correction

`crates/storage/tests/library_contract.rs` was changed at `a2250e9` from an unconditional `StageUnavailable` assertion to a both-branch assertion because the 2026-11-22 user ruling connected native staging on qualified volumes; the refusal branch still asserts `StageUnavailable`, the initial profile and zero `revisions/` entries, and the qualified branch asserts a prepared receipt plus reopen/`reconcile`; approved by the orchestrator as scope-mandated; `qualified_stage` is the positive guard and must not become a silent skip.
