# M1-R03 native results

Runtime evidence from 2026-11-22 (IST), code commit `29e587d`, base `ebd9e07`. `not run` is never a pass. No power-cut or VM evidence exists.

Close-out 2026-11-22: the Task 5 gate blocked native wiring. Native library staging is now disconnected from `Catalog::open`; the Btrfs library witness is `not run` and every corrected branch is marked below. No Task 6 native wiring is unlocked.

Environment: Linux 7.3.5-1-cachyos, x86-64, rustc 1.100.0, uid 1000 with `CapEff: 0` (ordinary user, no elevated token). tmpfs scratch: `/tmp` `rw,noatime,inode64,huge=advise`, fsid `b6b575e7…`, mount view 74, no Btrfs tree. Btrfs scratch: `/home` on `/dev/nvme0n1p2[/@home]` `rw,noatime,compress=zstd:1,ssd,discard=async,space_cache=v2,subvolid=257,subvol=/@home`, fsid `b68b24bc…`, view 61, tree 257. All fixtures are synthetic files under a disposable parent; no game, external root or elevated operation was touched.

| Proof case | Linux Btrfs | Linux tmpfs | Linux ext4 | Hosted `windows-2025` NTFS | Power |
|---|---|---|---|---|---|
| `native_no_replace_honors_concurrent_create` | ran 2026-11-22, passed | ran 2026-11-22, passed | not run | not run | — |
| `native_replace_records_actually_displaced_file` | ran 2026-11-22, passed | ran 2026-11-22, passed | not run | not run | — |
| `native_rejects_root_and_parent_substitution` | ran 2026-11-22, passed | ran 2026-11-22, passed | not run | not run | — |
| `native_detach_preserves_unknown_file` | ran 2026-11-22, passed | ran 2026-11-22, passed | not run | not run | — |
| `native_error_does_not_prove_no_change` | ran 2026-11-22, passed | ran 2026-11-22, passed | not run | not run | — |
| `native_namespace_survives_claimed_barrier` | ran 2026-11-22, passed (process-only, new create only) | ran 2026-11-22, passed (process-only, new create only) | not run | not run | Task 8, not run |
| `library_catalog_never_names_missing_payload` | not run (native library staging disconnected pending gate) | not run (native library staging disconnected pending gate) | not run | not run | Task 8, not run |
| `btrfs_cross_subvolume_copy_does_not_use_cross_subvolume_rename` | not run (subvolume teardown EPERM; two-subvolume `EXDEV`/substitution not run) | N/A | N/A | N/A | — |

## Per-row evidence

1. **No-replace create** — the child created `Data/textures/new.dds = edit\n` at the `BeforeNativeEffect` cut, before the port issued any rename. The port's recheck observed the parent identity/leaf and the now-present target and refused with `Conflict`; no native `renameat2` `EEXIST` was reached. Target retained `edit\n`; the private stage still held `mod\n`; sentinels unchanged.
2. **Replace with actual displacement** — child moved `winner.dds = base\n` to `old`, wrote `edit\n` at the public name and recorded its independent file identity. After the checkpoint the port observed an identity different from the frozen pre-image and refused cleanly: outcome `Conflict`, `edit\n` remained public, stage `mod\n` intact, and both independent recovery copies (`original` and `step`) held `base\n` at separate operation-owned refs. The harness accepts this refusal branch or the exact-retention branch; the refusal branch was the observed one.
3. **Root and parent substitution** — the child moved `Data/textures` outside `Data` and put a symlink in its place. Re-resolution through the live binding failed (`RESOLVE_NO_SYMLINKS`/beneath), so the port returned `Unavailable(OutsideBoundary|TargetUnavailable)` or `Conflict` before any rename. `moved-parent/winner.dds` retained `base\n`, the stage retained `mod\n`, and no outside sentinel changed.
4. **Checked detach** — same swap race with no staged output: outcome `Conflict`, the child's `edit\n` remained at the public name, no retained slot was created, and nothing was deleted or settled as a removal.
5. **Error is not "no change"** — injection returned `Fault::LoseAcknowledgement` after a successful exchange. The port reobserved and returned `Ambiguous` with the new target `mod\n`, the canonical retained slot holding `base\n`, and the logical stage absent. Genuine missing-target and read-only-metadata (`user.r03` xattr) refusals were exercised separately and were not reported as success. No claim is made for Win32 1175/1176/1177.
6. **Namespace barrier** — process-only cuts for a **new create only**. Before publish the target was absent; after publication and before the barrier the child process saw the new name; after the claimed Linux barrier a reopened verifier saw the same object identity and bytes. The replace, detach and Restore barriers are `not run`. No power-loss or VM-restart claim is made.
7. **Catalog never names a missing payload** — `not run`. `Catalog::open` now always uses the `Unavailable` stage; native library staging is disconnected pending the Task 5 gate, so the real catalog cannot reach a native receipt on any volume. A `StageUnavailable` readiness probe is all the witness can report; it is not a pass.
8. **Btrfs cross-subvolume** — `not run`. `btrfs subvolume create` succeeded and `btrfs subvolume delete` returned `EPERM` ("Operation not permitted"), which the runbook treats as not run. The scratch path was retained; no shim or other filesystem was substituted. The matching-inode substitute-subvolume assertion and the `EXDEV`/cross-subvolume copy check belong to this unrun two-subvolume case; they were not exercised.

## Commands

- `python tests/platform/m1/native-publication/run.py /tmp/opencode/r03-run` (tmpfs): exit 0; 8 passed, 1 ignored; library case reported `R03 NOT RUN: catalog native receipt unavailable on this volume`; subvolume case reported not run.
- `python tests/platform/m1/native-publication/run.py /home/parth/.cache/r03-run3` (Btrfs): exit 0; 8 passed, 1 ignored; the library case then ran the real catalog cuts against volume-selected native staging. That wiring is now disconnected pending the gate, so this observation is superseded and does not qualify the current tree; subvolume case reported teardown EPERM.
- `cargo test --locked -p marduk-platform --test native_publication`: compiles and passes; without `MARDUK_R03_SCRATCH` every runtime case prints `R03 NOT RUN`.
- `cargo test --locked -p marduk-storage --test library_contract` (`TMPDIR=/tmp`): 5 passed; the production stage is `Unavailable` on every volume.
- `cargo check --locked --target x86_64-pc-windows-msvc -p marduk-platform --tests`: passed; this is compile-only, not NTFS runtime.
- `cargo fmt --all -- --check`, `cargo build --workspace --locked`, `cargo test --workspace --locked`: passed.

The frozen `run.py` builds its probe with only `.rlib` artifact directories. In this workspace the `zeroize_derive` proc-macro artifact lives in a separate build directory, so the probe first failed with `E0463: can't find crate for marduk_storage`. Symlinking the hashed proc-macro `.so` into `target/debug` (already a search path) let the frozen script complete unchanged. The harness assertions themselves were not modified; no assertion failed.

## Residual closure (2026-11-22)

Adds `crates/platform/tests/native_proof_evidence.rs` detach witnesses, `library_stage.rs` error classification and a private `revisions/` mode. Frozen harness and oracle unchanged.

### Library staging (disconnected 2026-11-22)

The Task 5 gate blocked native library wiring. `Catalog::open` now always uses `StageAdapter::Unavailable`, so `prepare_import` returns `StageUnavailable` on every volume, including Btrfs. The native adapter and its proof code remain for the gate to qualify; the only retained entry point is the private, unused `Catalog::open_native` reservation. There is no public injection path or feature flag.

The Btrfs library witness is therefore `not run` in the current tree. An earlier revision selected native staging from the catalog root's filesystem magic (Btrfs only, `crates/platform/src/linux.rs:904`); those observations are historical and do not qualify the disconnected code.

| Suite / test | Required volume | Exact run condition | Observed |
|---|---|---|---|
| `crates/storage/tests/library_contract.rs` (5 tests), including `unqualified_production_catalog_refuses_preview_without_visible_changes` (`:57`) and `second_catalog_observes_the_active_writer_gate` (`:131`) | any | `TMPDIR=/tmp cargo test --locked -p marduk-storage --test library_contract` | 5 passed |
| `library_catalog_never_names_missing_payload` (`tests/platform/m1/native-publication/library.rs:134`) | any | `python tests/platform/m1/native-publication/run.py <scratch>` | `R03 NOT RUN: catalog native receipt unavailable on this volume`; not a pass |
| `cargo test --workspace --locked` | any | `TMPDIR=/tmp cargo test --workspace --locked` | every suite passed |
| `crates/storage/src/library/contract_tests.rs` (11 unit tests, frozen oracle) | any | `Catalog::open_for_test` forces the fixture stage | 11 passed under `TMPDIR=/tmp` |

There is no longer a volume-dependent difference: the frozen `library_contract` reports the same 5 passes under a Btrfs `TMPDIR` because production staging is disconnected everywhere. The earlier Btrfs negative evidence applied to volume-selected staging and is superseded.

### Detach and classification witnesses

`MARDUK_R03_SCRATCH` on Btrfs and tmpfs, `--test-threads=1`: `cargo test --locked -p marduk-platform --test native_proof_evidence` → 7 passed on each volume.

- `detach_rejects_wrong_retain_slot_without_touching_target`: `BackupSlot::Original` as the detach retain returns `Conflict`, the target keeps `base\n`, and the operation-owned `displaced` slot stays absent.
- `detach_native_error_with_missing_target_and_occupied_slot_is_ambiguous`: the target disappears and the displaced slot is written at `NativeRename`; `renameat2` fails, reobservation reports target `Absent` with a non-empty retained slot, and the outcome is `Ambiguous`, never `Observed`.
- `library_stage.rs:210` now maps a refused/exhausted `create_new` (permission, ENOSPC, I/O) to `StageUnavailable` and only `AlreadyExists` to `StageChanged`.
- `library_stage.rs:274` maps `ArchiveError::Io` from `admit_zip` to `StageUnavailable`; every other admission error stays `StageChanged`. No new test drives either branch: both need an injected syscall failure, so only the classification code changed.
- `Catalog::open` creates `revisions/` with mode `0700` (`private_directory`); `revisions_directory_is_private_to_the_owner` asserts `0700` on create and after reopen.


### Handle-relative private staging (2026-11-22)

Base `8110c38`. `NativeStage` now keeps every archive and payload effect on the verified revision handle: parents are created with `mkdirat(0700)`, leaves with `openat2(O_CREAT|O_EXCL|O_NOFOLLOW)`, and hashing, listing, fsyncs and reclaim unlinks resolve beneath the handle with `RESOLVE_BENEATH|RESOLVE_NO_SYMLINKS|RESOLVE_NO_XDEV`. The revision name is removed through the bound `revisions/` handle. This closes the path-join window for bytes; it does not qualify publication.

- `MARDUK_M1_BTRFS_SCRATCH=/var/tmp cargo test -p marduk-platform --test library_boundary` — 8 witnesses ignored without `--ignored` (none pass silently); with `--ignored --test-threads=1`, 8 passed.
- New witnesses redirect a staged revision after its handle was opened and require `StageChanged`/`StageUnavailable` from `reobserve` and `reclaim` with the redirect target untouched and the original revision retained.
- `TMPDIR=/tmp cargo test -p marduk-platform`, `TMPDIR=/tmp cargo test -p marduk-storage`, `cargo fmt --all -- --check`, `cargo build --workspace --locked`, `TMPDIR=/tmp cargo test --workspace --locked` — passed.
- Windows compile check passed; Windows/NTFS staging remains `Unsupported(Configuration)`. `Catalog::open` remains `StageUnavailable` and no native Apply is connected.

Final-window rename containment remains the blocker: publication renames still lack an expected-parent primitive. No native wiring is unlocked.
