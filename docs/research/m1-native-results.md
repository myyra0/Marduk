# M1-R03 native results

Running record of the native evidence behind M1's availability decisions. The matrices below
are parsed by `crates/core/tests/availability_guard.rs`: a passing cell names the commit it ran
at, and the build fails when the record and the code disagree. Dates are omitted; the revision
is the identity of a run. Start with [M1 status](../m1/README.md) if you want the summary.

`not run` is never a pass. No qualifying power-cut result exists. [ADR 0004](../adr/0004-windows-writes-use-standard-flush.md) narrows the Windows barrier to
standard flushing, which excludes power-cut recovery from the claim.

Environment: Linux 7.3.5-1-cachyos, x86-64, rustc 1.100.0, uid 1000 with `CapEff: 0` (ordinary user, no elevated token). tmpfs scratch: `/tmp` `rw,noatime,inode64,huge=advise`, fsid `b6b575e7…`, mount view 74, no Btrfs tree. Btrfs scratch: `/home` on `/dev/nvme0n1p2[/@home]` `rw,noatime,compress=zstd:1,ssd,discard=async,space_cache=v2,subvolid=257,subvol=/@home`, fsid `b68b24bc…`, view 61, tree 257. All fixtures are synthetic files under a disposable parent; no game, external root or elevated operation was touched.

| Proof case | Linux Btrfs | Linux tmpfs | Linux ext4 | Hosted `windows-2025` NTFS | Power |
|---|---|---|---|---|---|
| `native_no_replace_honors_concurrent_create` | ran at `29e587d`, passed | ran at `29e587d`, passed | not run | not run | — |
| `native_replace_records_actually_displaced_file` | ran at `29e587d`, passed | ran at `29e587d`, passed | not run | not run | — |
| `native_rejects_root_and_parent_substitution` | ran at `29e587d`, passed | ran at `29e587d`, passed | not run | not run | — |
| `native_detach_preserves_unknown_file` | ran at `29e587d`, passed | ran at `29e587d`, passed | not run | not run | — |
| `native_error_does_not_prove_no_change` | ran at `29e587d`, passed | ran at `29e587d`, passed | not run | not run | — |
| `native_namespace_survives_claimed_barrier` | ran, passed (process-only, new create only) | ran, passed (process-only, new create only) | not run | narrowed (ADR 0004: standard flush, power cut not covered) | Task 8, not run |
| `library_catalog_never_names_missing_payload` | not run (staging wired on this volume; catalog witness not run) | not run (staging wired on this volume; catalog witness not run) | not run (unqualified; `StageUnavailable`) | not run (catalog witness not run) | Task 8, not run |
| `btrfs_cross_subvolume_copy_does_not_use_cross_subvolume_rename` | not run (subvolume teardown EPERM; two-subvolume `EXDEV`/substitution not run) | N/A | N/A | N/A | — |

**Evidence scope:** The root/parent substitution row covers refusal when substitution was visible at the last live check. It does not demonstrate prevention inside the final check-to-syscall window.

**Narrowed is not a pass.** ADR 0004 accepts standard filesystem flushing on Windows and drops the claim that an interrupted deploy is always recoverable. The NTFS barrier cell therefore records a decision, not a run: the barrier and crash rows stay unrun, and no power-cut result exists on any configuration. The separate NTFS composition run (below) opens the deploy and restore routes without changing that narrowed barrier row.

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

## Residual closure

Adds `crates/platform/tests/native_proof_evidence.rs` detach witnesses, `library_stage.rs` error classification and a private `revisions/` mode. Frozen harness and oracle unchanged.

### Library staging (disconnected, superseded below)

The Task 5 gate blocked native library wiring at that revision: `Catalog::open` used `StageAdapter::Unavailable` on every volume. The Btrfs library witness was therefore `not run` there, and the earlier Btrfs-only observation (selected from the catalog root's filesystem magic, `crates/platform/src/linux.rs:904` at the time) did not qualify the disconnected code. The frozen `library_contract` reported the same 5 passes under a Btrfs `TMPDIR` because staging was disconnected everywhere. This paragraph is history; the next section is the current state.

### Qualified library staging (connected

Tested at commit `a2250e9`, base `252ce26`.
Continuation of Task 5 after the ruling. `Catalog::open` now selects native staging only when `marduk_platform::library_volume_qualified` reports a volume class the availability record proves at the catalog layer: **Linux Btrfs** and **Linux tmpfs**. The platform reads the observed private library root with the crate's existing `fstatfs` path (`crates/platform/src/linux.rs`), not a `statfs` type string, and reports only the class; `crates/core/src/qualification.rs` decides availability. The catalog refuses with `StageUnavailable`:

- Linux ext4 and every other unrecognised magic, including `FUSE` (ntfs-3g) and `ntfs3`.
- Windows product availability: the staging adapter is implemented and witnessed on native NTFS (below), but the record proves only the staging and primitive layers there, so `library_volume_qualified` and `effects_volume_qualified` stay `false` and catalog imports and deploys remain disconnected.
- A qualified volume whose private boundary will not bind: a redirected root or `revisions/`, an owner or mount mismatch, or a two-subvolume/`EXDEV` layout with no evidence. `NativeStage::open` refusal is final; it is never retried through a weaker adapter.

Wired: private staging (private archive snapshot, admitted member set, hashed payload under `revisions/<id>/`, `sync_revisions` before the receipt) and catalog publication of a prepared import into the library (SQLite membership only, after native re-observation). Separate from library staging, Task 6 connected the native copy adapter for game-directory preview, Apply and Restore in the workspace `proof_*` lane on qualified volumes. Client `dispatch` serves deploy and restore where the record proves the composition on the installation root's volume class (Linux Btrfs and tmpfs); see [Client-visible deploy and restore](#client-visible-deploy-and-restore-. Crash and power-cut behaviour is deferred, and no power-cut claim exists.

| Suite / test | Required volume | Exact run condition | Observed |
|---|---|---|---|
| `crates/storage/tests/qualified_stage.rs::qualified_volume_stages_the_payload_through_the_production_catalog` | tmpfs or Btrfs | `TMPDIR=/tmp cargo test --locked -p marduk-storage --test qualified_stage`; also run with `TMPDIR=/var/tmp` | passed on tmpfs and Btrfs; asserts the volume class instead of skipping; payload read back at `revisions/<id>/payload/textures/synthetic.dds`, revision dir `0700`, `reconcile` re-observes the receipt |
| `crates/storage/src/library/stage_tests.rs::refused_volume_configuration_keeps_the_stage_unavailable` | any | `TMPDIR=/tmp cargo test --locked -p marduk-storage` | passed; `StageUnavailable`, no import row, no `revisions/` entry |
| `crates/platform/src/linux_root_tests.rs` predicate tests (5) | any | `cargo test --locked -p marduk-platform --lib` | accept Btrfs/tmpfs; refuse ext4, FUSE/NTFS3 and unknown magic |
| `crates/storage/tests/library_contract.rs` (5 tests) | any | `TMPDIR=/tmp cargo test --locked -p marduk-storage --test library_contract`; also with `TMPDIR=/var/tmp` | 5 passed on both; the volume-dependent test is now `production_stage_follows_the_qualified_volume` (refusal on unqualified, staging plus re-observation on qualified) |
| `crates/storage/tests/production_refusal.rs` | any | `TMPDIR=/tmp cargo test --locked -p marduk-storage --test production_refusal` | 1 passed; an unprovable seeded stage row is never published on either adapter |
| `crates/platform/tests/library_boundary.rs` (10 witnesses) | Btrfs | `MARDUK_M1_BTRFS_SCRATCH=/var/tmp cargo test -p marduk-platform --test library_boundary -- --ignored --test-threads=1` | 10 passed; without `--ignored` 10 ignored, none silently passing |
| `crates/storage/src/library/contract_tests.rs` (10 unit tests, frozen oracle at `57de606`) | any | `Catalog::open_for_test` forces the fixture stage | 10 passed under `TMPDIR=/tmp`; the two post-freeze witnesses live in the unfrozen `extra_contract_tests.rs` |
| `crates/workspace/tests/workflow.rs::import_preview_and_apply_round_trip_on_a_qualified_volume` | tmpfs or Btrfs on Linux, NTFS on Windows | `TMPDIR=/tmp` and `TMPDIR=/var/tmp cargo test --locked -p marduk-workspace --test workflow -- --exact import_preview_and_apply_round_trip_on_a_qualified_volume`; same test on the Windows VM | passed on tmpfs and Btrfs at `76f7850`, and on NTFS at `76f7850`; preview stages, apply publishes revision 1 with one member, replay returns the same snapshot. The test early-returns on an unqualified volume, so only these qualified runs are evidence |
| `library_catalog_never_names_missing_payload` (`tests/platform/m1/native-publication/library.rs:134`) | tmpfs or Btrfs | `python tests/platform/m1/native-publication/run.py <scratch>` | `not run` in this continuation; the table row above stays `not run` |

Library-staging witnesses. `import_preview_and_apply_round_trip_on_a_qualified_volume` is the catalog witness the record proves for the import route, because it publishes; the staging-only witnesses below remain recorded at their own rung. `windows_catalog_stages_a_payload_beside_its_root_pin` is the NTFS staging witness; the eight `library_staging_witnesses` rows are the recorded staging suite the record names.

| Staging witness | Linux Btrfs | Linux tmpfs | Windows NTFS |
|---|---|---|---|
| `import_preview_and_apply_round_trip_on_a_qualified_volume` | ran at `76f7850`, passed (published revision 1, one member) | ran at `76f7850`, passed (published revision 1, one member) | ran at `76f7850`, passed (published revision 1, one member); this row, not the staging-only one, is what the import availability record cites |
| `qualified_volume_stages_the_payload_through_the_production_catalog` | ran at `a2250e9`, passed | ran at `a2250e9`, passed | not run |
| `windows_catalog_stages_a_payload_beside_its_root_pin` | not run | not run | ran at `62b9b94b`, passed |
| `staged_revision_keeps_real_bytes_private_and_reobserves_after_reopen` | not run | not run | ran at 367ca69, passed |
| `redirected_revisions_is_refused_without_writes` | not run | not run | ran at 367ca69, passed |
| `redirected_revision_path_is_refused_without_writes` | not run | not run | ran at 367ca69, passed |
| `redirected_payload_during_owner_record_is_refused_without_writes` | not run | not run | ran at 367ca69, passed |
| `reclaim_removes_a_verified_private_stage` | not run | not run | ran at 367ca69, passed |
| `reobserve_refuses_a_changed_bound_entry` | not run | not run | ran at 367ca69, passed |
| `reobserve_refuses_an_added_payload_file` | not run | not run | ran at 367ca69, passed |
| `reobserve_refuses_a_revision_redirected_while_unbound` | not run | not run | ran at 367ca69, passed |

The runtime evidence behind the accepted magic set is the tmpfs/Btrfs effect table at the top of this document plus the catalog staging witnesses above. No ext4 catalog run exists; native NTFS catalog and composition rows are recorded in [Windows composition run](#windows-composition-run-. `VolumeClass::Other` is the catch-all: a new filesystem needs a `volume_class` mapping branch, a `PROOFS` row and a staging-matrix row; an unmapped magic stays unproven for every capability (`crates/platform/src/linux_root_tests.rs`).

Windows product availability follows the record: NTFS import is proven at the catalog layer and NTFS deploy/restore at the composition layer by the run below, while every other Windows volume class stays refused. The private staging adapter itself is implemented and was re-run on NTFS after the review fixes:

- Tree: `7bcd2c1` plus the staging review fix. VM: Windows 10 `DESKTOP-4NP2KM5`, `rustc 1.100.0`. Scratch volume `C:` reported `File System Name : NTFS` (`fsutil fsinfo volumeinfo C:`).
- `cargo test -p marduk-platform --test library_staging_witnesses -- --nocapture` → `running 8 tests`, `test result: ok. 8 passed; 0 failed`, including `reobserve_refuses_an_added_payload_file`.
- Privacy was asserted by an independent `GetKernelObjectSecurity` DACL scan (not the adapter's `private_dacl`) for `revisions/<id>`, `payload/`, `payload/textures/` and `payload/textures/synthetic.dds`: protected DACL, owner equal to the token's default owner, owner-only ACEs, the object ACE plus the inherit-only descendant ACE on every directory, and exactly one non-inherit-only ACE on the file. `icacls` showed `BUILTIN\Administrators:(F)` plus `BUILTIN\Administrators:(OI)(CI)(IO)(F)` on the revision and `BUILTIN\Administrators:(F)` on the file; the SSH token is elevated, so Administrators is the token owner.
- The redirect witnesses took the `symlink_dir` branch (logged by `link_directory`), not the `mklink /J` fallback, under the elevated SSH token; the junction branch is unexercised. That is an evidence gap, not a pass.
- `cargo test --workspace --locked` on the VM → exit 0, no failures. Linux `TMPDIR=/tmp cargo test --workspace --locked`, `cargo fmt --all -- --check`, `cargo build --workspace` and the MSVC-target `cargo check -p marduk-platform --all-targets --target x86_64-pc-windows-msvc` also passed.

Deploy-level NTFS rows remain `not run`; only the private staging adapter is evidenced here.

### Composition round trip

`crates/workspace/tests/native_deployment.rs::qualified_native_round_trip_retains_backups_and_restores_the_original_tree`
drives the production `Workspace` — the real `Catalog`, `OperationJournal` and
`NativeEffects` — through setup, ZIP import, member order, preview, apply,
restart, plan replay and restore on a qualified volume. Preview, Apply and
Restore run through the `proof_*` methods, so this witness stays independent of
the client availability gate. This is the composition rung: the whole service
composed and ran; the client-visible run is recorded separately below.

### Client-visible deploy and restore

`crates/workspace/tests/native_deployment.rs::dispatch_deploy_and_restore_through_the_whole_composition`
drives `deploy_preview`, `deploy_apply` and `restore` through `Workspace::dispatch`
on a qualified volume. Availability is a record query: deploy and restore require
a `NativeEffects`/`Deploy` proof at the composition rung or above for the
installation root's volume class (`crates/core/src/qualification.rs`). The
client-visible rung cannot be a precondition of the gate that enables the first
client-visible run, so `Layer::Deploy` is recorded after that run.

The witness asserts the reviewed plan, a completed apply with no blocker, the
deployed bytes and staged metadata, the status ref for the actually displaced
original with its bytes and metadata, the committed generation after a restart,
a completed restore that brings back the original tree and metadata, and a second
restore answering `NoActiveDeployment` with the tree unchanged.
`unqualified_installation_root_keeps_client_deploy_and_restore_refused` renames the
recorded installation root without a replacement; identity re-verification fails first,
so status, preview, apply and restore all refuse with `TargetUnavailable` rather than a
volume verdict. It builds its fixture on a qualified volume, so it stays `#[ignore]` and
runs only where `TMPDIR` is tmpfs or Btrfs.

Runs at `97c03d9` (`cargo test --locked -p marduk-workspace --test
native_deployment -- --ignored --test-threads=1`, 6 passed on each volume):

- tmpfs (`TMPDIR=/tmp`).
- Btrfs (`TMPDIR=/var/tmp`).

Crash and power-cut behaviour for Linux Btrfs and tmpfs is not covered by these
runs and remains deferred; the process-restart witnesses above are not
power-loss evidence. The NTFS composition and process-restart run is recorded in
[Windows composition run](#windows-composition-run-; it is not
power-loss evidence either.

The separate `deploy_evidence` test and its `evidence` feature entry points were
deleted. That build could force the stage open and serve a wire import while the
product import gate was closed, opened the root in a different order from
production, and duplicated this witness with a weaker plan-replay check.

The witness now also asserts the outside-plan sentinel (`user\n`) is unchanged,
`blocker.is_none()` for apply and restore, the second `proof_recover_pending` is
a no-op, and the restore clears the generation. For the actually displaced
object it finds the status `Displaced` ref for the replaced ordinal and reads
`base\n` from the sealed retain location (the canonical `displaced` slot, or the
Linux exchange alias in `stage`), then confirms restore consumes the deployed
object into its own sealed retain slot. The earlier harness only read the
pre-image `original` copy, so a broken retain path could pass.

`TMPDIR=/tmp` (tmpfs) and `TMPDIR=/var/tmp` (Btrfs) `cargo test -p
marduk-workspace --locked --test native_deployment -- --ignored
--test-threads=1` → 5 passed on each at `af0fc6d`.
The earlier NTFS composition attempt stopped at a sharing violation; the fixed
run is recorded below.

### Windows composition run

Code at `62b9b94b`, Windows 10 `DESKTOP-4NP2KM5`, `rustc 1.100.0`. The
synthetic scratch and installation root were under `%TEMP%` on `C:`
(`fsutil fsinfo volumeinfo C:` reports `File System Name : NTFS`). The run used
`schtasks /ru admin /rp 0000` because an SSH session kills the build; the
repository was synced with `git archive 62b9b94b`, `scp` and `tar -xzf` over
`C:\marduk`.

- `cargo test -p marduk-workspace --locked --test native_deployment -- --ignored
 --nocapture --test-threads=1` → `running 1 test`,
 `test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;
 finished in 0.27s`, `WITNESS_EXIT_0`.
 `qualified_native_round_trip_retains_backups_and_restores_the_original_tree`
 drove the production `Catalog`, `OperationJournal` and `NativeEffects`
 through setup, ZIP import (production catalog with native staging), member
 order, preview, apply, restart, plan replay and restore.
- Import reached the private stage while the catalog's app-root handle stayed
 open. The two opens now coexist because the pinned app root shares read and
 write (`FILE_SHARE_READ | FILE_SHARE_WRITE`) and denies delete sharing. The
 dedicated witness at the same revision,
 `cargo test -p marduk-storage --locked --test windows_catalog_staging --
 --nocapture --test-threads=1`, ran 1 passed; 0 failed; 0 ignored; 0 measured;
 0 filtered out; finished in 0.04s (`STAGING_EXIT_0`): `Catalog::open` on NTFS
 reported `stage_available()`, prepared an archive, read its payload back from
 `revisions/<id>/payload/textures/synthetic.dds`, and `reconcile` re-observed
 the receipt.
- Apply's first publish created the destination name while the identity-pinned
 target-parent handle stayed open. The parent pin now shares read and write and
 denies delete sharing, so the kernel's own destination open succeeds and the
 move completes.
- The deny-delete claim still holds with write sharing enabled:
 `crates/platform/tests/windows_root_regressions.rs` ran 7 passed, 1 ignored
 (`short_name_alias_is_never_an_absent_or_admitted_leaf`, needs 8.3 scratch) at
 the same revision, including `pinned_ancestor_shares_write_but_refuses_delete`
 (a write handle succeeds; renaming the pinned directory returns Win32 32) and
 `pinned_target_parent_accepts_a_new_destination`.
- `cargo test -p marduk-storage --locked --test catalog_sharing` → 3 passed at
 the same revision: `catalog_root_pin_and_native_stage_coexist`,
 `catalog_and_journal_open_together`, and the existing schema-sharing witness.

Exact composed effects observed by the witness: imported members are
`textures/winner.dds = b\n` and `textures/a-only.dds = only-a\n`; after apply the
public tree holds `winner.dds = b\n`, `a-only.dds = only-a\n`,
`untouched.txt = keep\n` and `outside-plan.txt = user\n`; the displaced original
`winner.dds = base\n` is retained at the operation-owned displaced slot; after
restart the committed generation replays without reapplying; restore returns
`winner.dds = base\n` and the initial tree byte-for-byte; a second restore is
`NoActiveDeployment` with the tree unchanged. Windows metadata preservation is
not asserted by this witness (`set_mode`, `mode_of`, `mtime_of` and `owner_of`
are Unix-only), so no NTFS metadata row is claimed here; the separate
`effects_primitives` copy witness remains unrun against this revision.

This is a composition-rung pass, not a client-visible run and not power-cut
evidence. The availability record now proves NTFS import at the catalog layer
and NTFS deploy/restore at the composition layer; the narrowed barrier row above
stays a decision.

### Scoped metadata witnesses (`af0fc6d`)

`qualified_native_round_trip_retains_backups_and_restores_the_original_tree`
now gives the install target and the staged payload different admitted metadata
before preview, so equal fingerprints on identical fixtures cannot stand in for
preservation. The run used `TMPDIR=/tmp` (tmpfs) and `TMPDIR=/var/tmp` (Btrfs)
with `--ignored --test-threads=1 --nocapture`, 5 passed on each,:

| Observation (identical on tmpfs and Btrfs) | Value |
|---|---|
| Original target before apply | mode `0640`, mtime `(1500000000, 123456789)`, uid/gid `1000:1000` |
| Staged payload before apply | mode `0600`, mtime `(1600000000, 987654321)` |
| Public target after apply | bytes `b\n`, mode `0600`, mtime `(1600000000, 987654321)`, uid/gid `1000:1000` |
| Retained displaced original after apply | bytes `base\n`, mode `0640`, mtime `(1500000000, 123456789)`, uid/gid `1000:1000` |
| Public target after Restore | bytes `base\n`, mode `0640`, mtime `(1500000000, 123456789)`, uid/gid `1000:1000` |
| Second Restore | `NoActiveDeployment`, tree unchanged |

Each row asserts bytes next to the metadata comparison, so a metadata pass
cannot mask a byte failure and a byte pass cannot mask a metadata failure. This
is the composition layer: production catalog, journal and native adapter through
`proof_preview_deployment`/`proof_apply_deployment`/`proof_restore`, with client
`dispatch` still refusing deploy and Restore.

`visible_xattr_on_an_original_is_refused_before_any_write` attaches
`user.marduk` to the admitted original before preview. Preview refuses with
`UnsupportedTarget`; the game tree, original mode/mtime and all bytes are
unchanged, and no operation directory is created. The composed error carries no
`Metadata` reason detail (`DeployError::UnsupportedTarget` is a unit variant),
so the typed code is the whole refusal promise at this layer. The platform root
suite separately returns `UnsupportedTarget`/`Metadata` for the same condition.

Two limits belong to the contract, not to this run:

- A `user.*` xattr is **refused** metadata, not preserved metadata
 ([M1 metadata scope](../contracts/m1-root-verification.md#m1-metadata-scope-approved-).
 The differential witness therefore preserves the admitted fields
 (owner/group/mode/mtime) and proves the xattr class by refusal, which is what
 the policy says. No policy text was changed.
- `trusted.*` is unobserved by an ordinary user: `setxattr` returns `EPERM`
 (os error 1), so the excluded class cannot be attached for a runtime observation
 here. The contract already classes `trusted.*` as unobserved and outside the
 verified Restore guarantee; it does not promise a typed refusal for it. That
 sub-case stays `not run`; no privilege was enabled.

Windows staging: `crates/platform/tests/effects_primitives.rs::staged_copy_preserves_owner_group_dacl_attributes_and_last_write`
is a Windows-only witness for the copy used to stage a payload. It tightens the
source DACL to the current user, sets `FILE_ATTRIBUTE_HIDDEN` on top of the
default archive attribute, and sets a distinct last-write, asserts that this
metadata really differs from an ordinary file, then copies and asserts the
destination fingerprint (owner, group, DACL digest, attributes, last-write),
bytes and attributes match the source. It compiles for
`x86_64-pc-windows-msvc`. It is **not run**: the VM was power-cycled by another
experiment, so no Windows runtime, NTFS volume or deploy-level claim is made.
This witness is the primitive copy behind staging; the Windows row stays
`not run`.

### Detach and classification witnesses

`MARDUK_R03_SCRATCH` on Btrfs and tmpfs, `--test-threads=1`: `cargo test --locked -p marduk-platform --test native_proof_evidence` → 7 passed on each volume.

- `detach_rejects_wrong_retain_slot_without_touching_target`: `BackupSlot::Original` as the detach retain returns `Conflict`, the target keeps `base\n`, and the operation-owned `displaced` slot stays absent.
- `detach_native_error_with_missing_target_and_occupied_slot_is_ambiguous`: the target disappears and the displaced slot is written at `NativeRename`; `renameat2` fails, reobservation reports target `Absent` with a non-empty retained slot, and the outcome is `Ambiguous`, never `Observed`.
- `library_stage.rs:210` now maps a refused/exhausted `create_new` (permission, ENOSPC, I/O) to `StageUnavailable` and only `AlreadyExists` to `StageChanged`.
- `library_stage.rs:274` maps `ArchiveError::Io` from `admit_zip` to `StageUnavailable`; every other admission error stays `StageChanged`. No new test drives either branch: both need an injected syscall failure, so only the classification code changed.
- `Catalog::open` creates `revisions/` with mode `0700` (`private_directory`); `revisions_directory_is_private_to_the_owner` asserts `0700` on create and after reopen.

### Handle-relative private staging

Base `8110c38`. `NativeStage` now keeps every archive and payload effect on the verified revision handle: parents are created with `mkdirat(0700)`, leaves with `openat2(O_CREAT|O_EXCL|O_NOFOLLOW)`, and hashing, listing, fsyncs and reclaim unlinks resolve beneath the handle with `RESOLVE_BENEATH|RESOLVE_NO_SYMLINKS|RESOLVE_NO_XDEV`. The revision name is removed through the bound `revisions/` handle. This closes the path-join window for bytes; it does not qualify publication.

- `MARDUK_M1_BTRFS_SCRATCH=/var/tmp cargo test -p marduk-platform --test library_boundary` — 8 witnesses ignored without `--ignored` (none pass silently); with `--ignored --test-threads=1`, 8 passed. (The suite has since grown to 10 witnesses; the qualified re-run records 10 passed.)
- New witnesses redirect a staged revision after its handle was opened and require `StageChanged`/`StageUnavailable` from `reobserve` and `reclaim` with the redirect target untouched and the original revision retained.
- `TMPDIR=/tmp cargo test -p marduk-platform`, `TMPDIR=/tmp cargo test -p marduk-storage`, `cargo fmt --all -- --check`, `cargo build --workspace --locked`, `TMPDIR=/tmp cargo test --workspace --locked` — passed.
- Windows compile check passed; the NTFS staging capability is implemented and witnessed at the adapter layer below. The later NTFS composition run opens `Catalog::open` native staging and Native Apply/Restore through the record; see [Windows composition run](#windows-composition-run-.

**Known limitation (accepted by the user ruling):** On Linux and Windows, a same-privilege process can substitute the parent or leaf between the final live-handle check and the syscall. The [R02 contract](../contracts/m1-recovery.md#native-seam-that-r03-must-qualify) requires post-effect reobservation to detect divergence, retention of actually displaced bytes, and refusal to retry automatically on divergence. It does not claim prevention. A syscall error is not evidence of no change.

The ruling changes the qualification criterion; it supplies no runtime evidence. Native staging is now wired on Linux Btrfs, Linux tmpfs and Windows NTFS through the record. Game-directory publication through the native adapter is connected on those volume classes, and client Apply and Restore are served there because the record proves the composition. Power-cut behaviour remains unqualified.

## Native copy adapter (Task 6,

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

The workspace reaches the adapter through its `proof_*` methods and its client
routes. `dispatch` and status ask the availability record through one decision
point for the installation root's volume class; the routes are available where
the record proves the composition, and no real game directory was written.

All rows below re-ran on at `101b4f7` on both qualified Linux volume
classes.

| Witness | Linux Btrfs | Linux tmpfs | Linux ext4 | Windows NTFS |
|---|---|---|---|---|
| `qualified_native_round_trip_retains_backups_and_restores_the_original_tree` (workspace: setup → import → order → preview → apply → restart → replay → restore) | ran at `101b4f7`, passed; re-ran with scoped metadata at `af0fc6d`, passed | ran at `101b4f7`, passed; re-ran with scoped metadata at `af0fc6d`, passed | not run | ran at `62b9b94b`, passed |
| `visible_xattr_on_an_original_is_refused_before_any_write` (workspace preview) | ran at `af0fc6d`, passed (`user.*`); `trusted.*` not run | ran at `af0fc6d`, passed (`user.*`); `trusted.*` not run | not run | not run |
| `restart_after_apply_before_ack_rolls_back_and_never_reapplies` | ran at `101b4f7`, passed | ran at `101b4f7`, passed | not run | not run |
| `restart_after_record_undo_uses_the_sealed_stage_fingerprint` | ran at `101b4f7`, passed | ran at `101b4f7`, passed | not run | not run |
| `restart_mid_restore_continues_in_reverse_order` | ran at `101b4f7`, passed | ran at `101b4f7`, passed | not run | not run |
| `concurrent_edit_during_apply_conflicts_and_preserves_external_bytes` | ran at `101b4f7`, passed | ran at `101b4f7`, passed | not run | not run |
| `restore_time_external_edit_conflicts_and_preserves_external_bytes` | ran at `101b4f7`, passed | ran at `101b4f7`, passed | not run | not run |
| `recorded_locator_mismatch_is_a_typed_refusal` | ran at `101b4f7`, passed | ran at `101b4f7`, passed | not run | not run |
| `unqualified_installation_root_is_refused_before_any_write` | ran at `101b4f7`, passed (procfs installation) | ran at `101b4f7`, passed (procfs installation) | not run | not run |
| `unqualified_app_root_is_refused_before_any_write` | ran at `101b4f7`, passed (sysfs app root) | ran at `101b4f7`, passed (sysfs app root) | not run | not run |
| `unreadable_private_slot_is_unavailable_not_absent` | ran at `101b4f7`, passed | ran at `101b4f7`, passed | not run | not run |
| `prepared_private_artifacts_are_durable_and_unpublished` | ran, passed | ran, passed | not run | not run |
| `private_chain_is_synced_before_the_first_public_effect` | ran, passed | ran, passed | not run | not run |
| `sync_chain_stops_at_the_resolved_root_for_a_symlinked_library_spelling` | ran, passed | ran, passed | not run | not run |
| `unqualified_root_answers_a_claimed_plan_and_pending_restore` (workspace) | ran, passed | ran, passed | not run | not run |
| `clean_unqualified_root_answers_before_opening_a_native_port` (workspace) | ran, passed | ran, passed | not run | not run |
| `dispatch_deploy_and_restore_through_the_whole_composition` (client-visible preview, apply and restore) | ran at `97c03d9`, passed | ran at `97c03d9`, passed | not run | not run |
| `unqualified_installation_root_keeps_client_deploy_and_restore_refused` (same three routes) | ran at `97c03d9`, passed | ran at `97c03d9`, passed | not run | not run |

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
runtime evidence; see [Windows native effects](#windows-native-effects-implemented--runtime-not-run)
for the implemented surface and its unrun runtime rows. The final-window
substitution limitation in
[R02](../contracts/m1-recovery.md#native-seam-that-r03-must-qualify) is unchanged:
the adapter detects divergence after an effect and retains displaced bytes; it
does not claim prevention.

## Windows native effects (primitive and composition runtime)

The Windows arms of `crates/platform/src/effects.rs` implement the same
contract as the Linux arms. The six `effects_primitives` witnesses pass on the
hosted `windows-2025` job at `eb39be0` on real NTFS. The later NTFS
whole-composition run at `62b9b94b` exercises publish, replace, detach and
restore through the production adapter. Power-cut evidence still does not exist;
`not run` is never a pass.

| Primitive | Contract implemented | Linux runtime | Hosted `windows-2025` NTFS runtime | Power |
|---|---|---|---|---|
| `effects::copy_file` | `CREATE_NEW` destination, data stream, attributes, creation/access/last-write times, owner/group/DACL, `FlushFileBuffers`; an occupied destination maps to `CopyError::DestinationExists` (conflict), a lock to `CopyError::Sharing` (retryable `Io`), and only unreproducible metadata to `Unsupported(Metadata)`; a source DACL that grants write (concrete bits, `GENERIC_WRITE`, `GENERIC_ALL`, or fail-closed `MAXIMUM_ALLOWED`) to a principal other than the current user or the SYSTEM/Administrators baseline is refused before it reaches the private area, including inherited grants, and every post-create failure removes the partial file | `effects_primitives::copy_preserves_bytes_and_last_write_time_and_refuses_existing` passed (Linux) | ran at `eb39be0`, passed | — |
| `effects::publish` | one `MoveFileExW` with flags `0` (no `MOVEFILE_REPLACE_EXISTING`, no `MOVEFILE_COPY_ALLOWED`); an existing target fails and is left intact; the staged object's creation time is read through a bound handle before the move and restored through that handle after it, so a name vacated inside the NTFS tunneling window cannot re-apply the previous object's birth time, and a denied attribute write fails before either name changes | `effects_primitives::publish_refuses_an_existing_target_and_leaves_it_intact` passed; `publish_keeps_the_staged_birth_time_on_a_recently_vacated_name` failed before the fix and passes after it, native NTFS | ran at `eb39be0`, passed; tunneling witness ran, passed | — |
| `effects::replace` | exchange by two no-replace renames in production (target → `<operation>/displaced`, stage → target); a caller that retains at a different path adds a third rename from that slot; a cut after the first rename leaves the canonical `displaced` slot plus the staged object, and production recovery reverts a pending deploy or completes a pending restore from that shape (see [Post-gate fixes](#post-gate-fixes-); the staged object's file id, creation time, attributes and DACL are preserved, so the public target fingerprints as the sealed staged artifact; a failure returns an error, never `Ok` | `effects_primitives::replace_retains_the_displaced_file_and_keeps_the_staged_object` passed | ran at `eb39be0`, passed | — |
| `effects::detach` | one no-replace rename into the retain slot; an occupied slot fails, unknown bytes are never replaced | `effects_primitives::detach_preserves_an_unknown_file` passed | ran at `eb39be0`, passed | — |
| `effects::sync_directory` | `FlushFileBuffers` on a directory handle opened with `FILE_FLAG_BACKUP_SEMANTICS`; not a namespace or power-cut barrier | `effects_primitives::sync_directory_succeeds_on_a_real_directory` passed | ran at `eb39be0`, passed | not claimed |
| `effects::volume_qualified` | a record query: NTFS is true for the adapter and deploy routes where the composition proof exists; the NTFS probe (from `GetVolumeInformationByHandleW` on the opened root) supplies the class, not the decision | `effects_primitives::volume_qualification_reads_the_opened_root` passed (refusal branch) | not run | — |

The source-DACL witnesses `copy_refuses_concrete_file_write_data_in_source_dacl`,
`copy_refuses_generic_write_in_source_dacl` and
`copy_refuses_generic_all_in_source_dacl` are Windows-only (`#[cfg(windows)]`), and
`interrupted_replace_keeps_the_canonical_displaced_slot_and_resumes` was added
after `eb39be0`. Neither group is part of the `eb39be0` Linux or NTFS runs above,
and this record has no runtime result for them; they do not open an availability
route.

The NTFS adapter and deploy answer comes from the whole-composition row at
`62b9b94b` (see [Windows composition run](#windows-composition-run-),
not from the primitive `volume_qualified` witness above.

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
NTFS (the run that exposed and then verified the tunneling fix). The NTFS
whole-composition run at `62b9b94b` adds the adapter and deploy rows. The
namespace barrier remains narrowed by ADR 0004 rather than proven, so the record
opens Apply/Restore without claiming power-cut recovery. `effects_volume_qualified`
returns true on NTFS through the composition proof and stays false for every
other Windows volume class.

Commands run for this row set:

- `cargo check -p marduk-platform --all-targets --target x86_64-pc-windows-msvc` — passed (compile only).
- `cargo test -p marduk-platform` (host Linux) — passed, including 6/6 in `tests/effects_primitives.rs`.
- `cargo fmt --all -- --check` and `cargo build --workspace --locked` — passed.

### Windows NTFS namespace barrier review, base `495678a`)

**Not approved as a barrier; ADR 0004 narrows the guarantee instead.** For local
NTFS and an ordinary Windows token, flushing file data and then calling
`FlushFileBuffers` on the modified parent does not establish the namespace
durability required by [M1 recovery](../contracts/m1-recovery.md). [ADR
0004](../adr/0004-windows-writes-use-standard-flush.md) accepts standard
flushing without a power-cut guarantee. `Narrowed` in
`crates/core/src/qualification.rs` records that decision separately from
`PROOFS`; the guard rejects a runtime pass or route availability from a narrowed
cell. The separate composition proof at `62b9b94b` is what opens Windows deploy
and restore; it does not turn the narrowed barrier row into a pass.

**What the code does.** `effects::sync_directory` opens a directory for write
with `FILE_FLAG_BACKUP_SEMANTICS` and calls `File::sync_all` (on Windows,
`FlushFileBuffers`). Publish, replace and detach use same-volume, no-replace
`MoveFileExW` with flags `0`. At `6056ab3` the barrier's blanket Windows refusal
was removed. `effects::sync_bound_directory` flushes the changed parent, and
`NativeEffects::durability_barrier` returns `Durable` only when the reobserved
evidence is unchanged. There is no post-effect file flush: retained bytes were
stable before the effect and must not be reopened for write, while copies this
process created are flushed by `copy_file`. The current sequence is at
`0e194d6`; the NTFS composition run at `62b9b94b` exercised the earlier form
that still reopened retained files, and completed apply and restore. The change
remains a narrowed standard flush, not power-cut evidence.
Context7's `windows-sys` lookup was checked against the locked
`windows-sys-0.61.2/src/Windows/Win32/Storage/FileSystem/mod.rs` bindings;
[Rust's Windows file implementation](https://github.com/rust-lang/rust/blob/master/library/std/src/sys/fs/windows.rs)
shows the actual sync call.

**Windows contracts and limits.**

- [`CreateFileW`](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createfilew)
 documents `OPEN_EXISTING` and `FILE_FLAG_BACKUP_SEMANTICS` for directory
 handles. The flag enables the open; it doesn't override access checks unless
 the requisite backup/restore privileges are present.
 [`FlushFileBuffers`](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-flushfilebuffers)
 documents `GENERIC_WRITE`. An ordinary user can get that access in a personal
 directory whose DACL grants it; ownership or permission to add a file alone
 doesn't grant the whole mask (it includes `FILE_ADD_SUBDIRECTORY` and
 write-attribute rights). See [file access rules](https://learn.microsoft.com/en-us/windows/win32/fileio/file-security-and-access-rights).
 A narrower `FILE_ADD_FILE` mask has reportedly worked on NTFS
 ([RocksDB draft](https://github.com/meta/rocksdb/pull/15682)); that's not the
 Win32 contract. A raw volume flush is documented but requires administrative
 privileges. The upstream draft also reports that opening the volume-root
 directory for generic write can fail on a normal non-elevated token.
- [MS-FSA §2.1.5.7](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-fsa/0de7dc40-9627-437e-a4df-c4696cdc3d02)
 says the object-store directory flush persists the directory structure;
 [product note 80](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-fsa/4e3695bd-7574-4f24-a223-b4679c065b63)
 identifies NTFS as implementing it. That is good reason to test the candidate,
 but not a local Win32 mapping that specifies which preceding child-name
 changes and journal records a writable directory handle commits. The
 [Microsoft Q&A moderator's distinction](https://learn.microsoft.com/en-us/answers/a/13008957)
 is consistent with this reading; it isn't an API specification.
- [`MoveFileExW`](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw)
 says `MOVEFILE_WRITE_THROUGH` doesn't return until the file is moved on disk,
 but the explicit flush guarantee is for a move implemented as copy/delete
 (the flush is at the end of the copy). There is no comparably explicit
 power-loss guarantee for both bindings of an in-volume rename. Copy/delete
 is forbidden here. Adding only `MOVEFILE_WRITE_THROUGH` is a useful comparison
 arm, not a substitute barrier. `MOVEFILE_REPLACE_EXISTING` would not retain
 the displaced file. [`ReplaceFileW`](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-replacefilew)
 is a different API: `REPLACEFILE_WRITE_THROUGH` is unsupported, and even
 with a backup name the attribute merge breaks the staged fingerprint.
- [`NtFlushBuffersFileEx`](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/nf-ntifs-ntflushbuffersfileex)
 with flags `0` is the full flush form (cached data and metadata plus
 storage-cache synchronization) and requires write or append access. It
 doesn't specify a stronger name-binding guarantee. `FILE_DATA_ONLY` omits
 metadata and storage synchronization; `NO_SYNC` omits storage synchronization;
 `FILE_DATA_SYNC_ONLY` is invalid for directory handles. No flag cited here
 provides a better ordinary-token namespace barrier.
- [`FILE_FLAG_WRITE_THROUGH`](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createfilew)
 covers metadata that results from the write-through request, including an
 NTFS rename in Microsoft's description. Setting it on an unrelated open
 doesn't order a later pathname operation. Even a handle-based rename on a
 write-through handle deserves direct measurement, not inference from the
 flag. NTFS journal replay restores [filesystem consistency](https://learn.microsoft.com/en-us/windows-server/storage/file-server/ntfs-overview),
 not necessarily the latest visible name. Home metadata pages need not be
 durable if the complete redo is durable; submitting a device flush doesn't
 show that either reached it. A flush/FUA is also only as good as the
 [filter, controller and device cache path](https://learn.microsoft.com/en-us/windows-hardware/drivers/storage/querying-for-the-write-cache-property)
 that honors it.

**Upstream practice (not Marduk evidence).**

- [SQLite `os_win.c`](https://github.com/sqlite/sqlite/blob/master/src/os_win.c#L2180-L2220):
 `winSync` flushes the mapped view when present, then the file handle.
 [`winDelete`](https://github.com/sqlite/sqlite/blob/master/src/os_win.c#L4552-L4567)
 explicitly ignores `syncDir` on Win32; no reason or parent-flush guarantee
 is given there.
- [LMDB `mdb.c`](https://github.com/LMDB/lmdb/blob/mdb.master/libraries/liblmdb/mdb.c#L2897-L2935):
 `mdb_env_sync0` says the Windows commit normally uses write-through writes.
 Forced sync uses the data-file handle (`MDB_FDATASYNC` is `FlushFileBuffers`
 on Windows); mapped sync also flushes the view. Transaction publication
 updates database pages and metadata in the existing file, not a directory
 entry per commit; there is no directory sync in this path.
- [RocksDB `WinDirectory::Fsync`](https://github.com/facebook/rocksdb/blob/ff683b119cbf42aa616eaeb5fbcec22ee40defb3/port/win/io_win.cc#L1085-L1088)
 is a no-op returning success. Its [directory open](https://github.com/facebook/rocksdb/blob/ff683b119cbf42aa616eaeb5fbcec22ee40defb3/port/win/env_win.cc#L574-L607)
 requests zero access for metadata queries, not write access. File sync
 uses `FlushFileBuffers`; `RenameFile` uses `MOVEFILE_REPLACE_EXISTING`
 without write-through. The draft to add a directory flush is not merged.
- [LevelDB `env_windows.cc`](https://github.com/google/leveldb/blob/main/util/env_windows.cc#L312-L326)
 flushes the file and explicitly skips parent sync, saying creation updates
 the metadata. That comment isn't a durability guarantee. The Windows
 rename uses `MoveFileA`, with `ReplaceFileA` as fallback for an existing
 destination; the [directory-flush proposal](https://github.com/google/leveldb/pull/1388)
 is not merged.
- [libuv `src/win/fs.c`](https://github.com/libuv/libuv/blob/v1.x/src/win/fs.c#L2340-L2356):
 `uv_fs_fsync`/`fdatasync` use `FlushFileBuffers` on the supplied file
 descriptor; `uv_fs_rename` uses `MoveFileExW(MOVEFILE_REPLACE_EXISTING)`.
 It doesn't sync the parent in rename or promise a family of durable renames.
- [sled 0.34.7](https://github.com/spacejam/sled/blob/v0.34.7/src/pagecache/iobuf.rs#L748-L757)
 syncs the log file; its [snapshot writer](https://github.com/spacejam/sled/blob/v0.34.7/src/pagecache/snapshot.rs#L556-L563)
 syncs the temporary snapshot then uses `std::fs::rename`, with no parent
 sync there on any OS. [redb's file backend](https://github.com/cberner/redb/blob/2fc790d14fb62f39137b13f7943df433bec082c2/src/tree_store/page_store/file_backend/optimized.rs#L331-L337)
 syncs the database file with `sync_data`; commits aren't a filesystem
 rename protocol and don't sync the parent.
- [Rust `atomicwrites`](https://github.com/untitaker/rust-atomicwrites/blob/master/src/lib.rs#L314-L334)
 syncs the temporary file, syncs parents on Unix, and substitutes
 `MoveFileExW(MOVEFILE_WRITE_THROUGH)` on Windows (also
 `MOVEFILE_REPLACE_EXISTING` for overwrite). [Rust `tempfile::persist`](https://docs.rs/tempfile/latest/tempfile/struct.NamedTempFile.html#method.persist)
 explicitly says it doesn't synchronize the file contents or containing
 directory. These are publication APIs, not precedents for the M1 recovery
 guarantee.

**What decided the refusal.** The published API scope leaves a gap, and
third-party [E17 block traces](https://github.com/takomitkm/leveldb/blob/main/docs/powercut/E17-RESULTS.md)
and [E21 ETW traces](https://github.com/takomitkm/leveldb/blob/main/docs/powercut/E21-RESULTS.md)
report a disk flush without the expected directory/MFT writeback on the
parent-directory path. Log records can be present without home pages;
the reports don't establish what Windows replay reconstructs for Marduk.
The same project's [E29–E32](https://github.com/takomitkm/leveldb/tree/main/docs/powercut)
report no reproducible stronger barrier from same-volume
`MOVEFILE_WRITE_THROUGH`, a write-through native rename handle, or
`NtFlushBuffersFileEx` flag 8 on the tested stacks. These are warnings, not
our qualification results. The earlier hard `virsh destroy` observation
left the new name and correct bytes with and without the parent flush.
Neither arm showed loss, and the cut did not control what host or device
caches could write back. That comparison is inconclusive.

**Deferred final experiment.** Use the [R03 cut oracle](../contracts/m1-native-proof.md)
and a separate cut driver; the existing process witness writes its
checkpoint in the test tree and measures visibility, not persistence.
Use a disposable local NTFS volume under a recorded ordinary token, Windows
build, DACL, and cache stack. Prepare a known stable baseline with unique names
and distinguishable bytes, identities and metadata. Run matched trials of
the same mutations with leaf/data sync only (negative control), with the
candidate parent flush added (all changed parents for cross-directory
moves), and with an elevated full-volume flush on the disposable volume
(positive control only). Keep timing and leaf sync the same in the two
ordinary-token arms. Test a new publish, multi-rename replacement with the
operation-owned displaced slot, detach/restore, and any newly created
directory bindings the operation relies on.

Capture writes, completions, FLUSH and FUA at the backing device with a
volatile-cache model, or use equivalent instrumentation and cut physical
power to the test storage. Mark syscall returns and journal acknowledgments
out of band, never by writing a marker to the tested volume. Reconstruct or
cut immediately before the namespace barrier and immediately after its
return, before unrelated writeback or acknowledgement. For reconstruction,
discard uncommitted writes: a completed FLUSH commits the writes it covers;
FUA commits that write, not all preceding writes. Inspect the durable
`$LogFile`, `$MFT` and directory index (`$I30`) stream updates, then let
Windows replay **copies** of those exact images. Verify the old and new
bindings, file IDs, hashes, metadata, retained bytes and journal state
against the frozen oracle. Include cuts between replacement moves and
before and after durable journal acknowledgement to separate legally
recoverable intermediate states from lost acknowledged changes.

A run discriminates only if the no-namespace-flush control loses or reverts
a name at a matched cut while the positive control retains it. Require the
candidate's complete expected state after successful barrier return at
those cuts; if it loses a name or retained object, reject it. If all arms
merely survive or the positive control fails, fix the cut setup rather
than recording a pass. Any result is scoped to the recorded configuration.

## Test-oracle correction

`crates/storage/tests/library_contract.rs` was changed at `a2250e9` from an unconditional `StageUnavailable` assertion to a both-branch assertion because the user ruling connected native staging on qualified volumes; the refusal branch still asserts `StageUnavailable`, the initial profile and zero `revisions/` entries, and the qualified branch asserts a prepared receipt plus reopen/`reconcile`; approved by the orchestrator as scope-mandated; `qualified_stage` is the positive guard and must not become a silent skip.

`tests/platform/m1/native-publication/subvolumes.rs` lost an adapter-side `AfterNativeEffect { raw_error: Some(EXDEV) }` assertion at `192a3b6`, when `native_proof::replace` began calling the promoted `effects::replace`, which returns `RootError` with no errno payload. The arm is unrepresentable on that path, so restoring it would be vacuous rather than a restored witness. Approved by the orchestrator as a coverage reduction with compensating controls: the direct `renameat2` control in the same file still asserts `EXDEV`, and the adapter cannot attempt a cross-volume rename because the recorded rename-domain precondition refuses it before any effect. Recorded here because an oracle change requires explicit approval and must not be silent. Widening effect errors to carry the raw OS error would restore the assertion if it is ever wanted.

## Post-gate fixes

Evidence added after the final whole-packet review blocked at `2d80c99`. Each row is a run, not a
derivation; `not run` is never a pass.

| Witness | Configuration | Command | Result | Commit |
|---|---|---|---|---|
| `foreign_stage_displacement_is_reported_and_stays_latched_after_reopen` | tmpfs and Btrfs | `TMPDIR=/tmp` and `/var/tmp cargo test -p marduk-storage --test native_effects -- --ignored` | 20 passed each; outcome `RecoveryConflict`, slot `stage`, latch survives reopen, bytes preserved | `e5c584c` |
| `windows_exchange_process_cut` | NTFS, native VM, production effect path in one process and recovery in a second | `cargo test -p marduk-workspace --test native_deployment -- --ignored --test-threads=1 windows_exchange_process_cut` | `ok`; cut state target absent, stage and `displaced` byte-exact, `alternate` absent; Deploy `rolled_back`, Restore `completed` | `3f58e98` |
| `read_only_original_deploys_and_restores`, `replace_refuses_a_denied_attribute_write_before_moving_anything` | NTFS re-run after the recovery change | as recorded above | both `ok` | `3f58e98` |
| `a_foreign_catalog_is_refused_until_the_owner_restores`, `the_record_survives_a_restart_and_a_torn_record_refuses`, `a_failed_deploy_that_rolls_back_leaves_no_record` | tmpfs and Btrfs, two catalogs over one installation | `TMPDIR=/tmp` and `/var/tmp cargo test -p marduk-workspace --test installation_lease` | 3 passed each; end-to-end CLI A-deploy/B-refuse/A-restore/B-deploy observed | `5d10180` |
| `the_key_is_stable_across_reopen_and_distinct_for_siblings`, `a_foreign_record_refuses_and_only_its_owner_releases_it`, `an_unparseable_record_is_a_typed_refusal_not_absence`, `an_unreadable_lease_directory_is_not_absent`, `a_lease_directory_with_group_access_is_refused`, `an_empty_directory_has_no_record_and_is_not_created`, `a_key_that_is_not_a_digest_is_refused` | Linux per-user lease directory | `cargo test -p marduk-platform --test installation_lease` | 7 passed | `5d10180` |
| `refused_import_leaves_no_private_snapshot_after_reconcile`, `owned_abandoned_reservation_is_reclaimed_and_unowned_bytes_are_quarantined`, `committed_and_prepared_imports_are_never_reclaimed`, `pre_fix_writing_row_is_handled_deterministically` | tmpfs and Btrfs | `TMPDIR=/tmp` and `/var/tmp cargo test -p marduk-storage --test import_recovery` | 4 passed each | `750d9d8` |
| `reconciliation_projects_ids_and_stable_reason_tokens`, `moved_installation_reports_target_unavailable_on_status_and_deploy`, `unqualified_app_volume_still_reports_unsupported_target_with_its_reason` | tmpfs | `cargo test -p marduk-workspace --locked` | all `ok`; `cargo test --workspace --locked` 345 passed, 0 failed | `6e214f7`, `f406c1c` |
| `parses LeaseUnreadable with its message instead of failing the transport`, `shows quarantined imports from the one startup reconciliation` | app test suite | `npm --prefix app test` | 78 passed | `0221afe` |
| installation lease, native Windows | NTFS, per-user lease directory | `cargo test -p marduk-platform --test installation_lease` and `cargo test -p marduk-workspace --test installation_lease -- --ignored --test-threads=1` | platform 7 passed; workspace 3 passed, including foreign refusal, restart survival and torn-record refusal; the gate is a separate `<key>.lock` object because Windows byte-range locks are mandatory | `3bd111a` |
| `publish_keeps_the_staged_birth_time_on_a_recently_vacated_name` | NTFS, a restore detaches the name and a deploy refills it inside the tunneling window | `cargo test -p marduk-platform --test effects_primitives -- --exact publish_keeps_the_staged_birth_time_on_a_recently_vacated_name` | failed before the fix (observed creation time is the detached object's), passes after it; `publish_refuses_a_denied_attribute_write_before_moving_anything` also passes | `196a477` |
| `cargo test -p marduk-workspace --test native_deployment -- --ignored` regression | NTFS after the lease and tunneling fixes | as above | 4 compiled Windows witnesses pass, including `windows_exchange_process_cut` | `196a477` |

Not run at this point: the hosted CI step that asserts `/dev/shm` is tmpfs. Power-cut durability, the Windows namespace-barrier experiment, a real Skyrim SE install, ext4 and Btrfs cross-subvolume remain as recorded in `docs/m1/limitations.md`.
| `a_live_plan_claim_survives_restore_and_recover`, `a_crashed_recovery_claim_is_cleared_when_the_journal_is_idle` | tmpfs and Btrfs | `TMPDIR=/tmp` and `/var/tmp cargo test -p marduk-workspace --test installation_lease -- --ignored --test-threads=1` | 7 passed each; a `plan:` claim survives an idle-journal restore and recover, and only a `recovery:` claim is cleared | `dfcbb09` |
