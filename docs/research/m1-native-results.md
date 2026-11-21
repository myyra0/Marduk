# M1-R03 native results

Runtime evidence from 2026-11-22 (IST), code commit `29e587d`, base `ebd9e07`. `not run` is never a pass. No power-cut or VM evidence exists.

Environment: Linux 7.3.5-1-cachyos, x86-64, rustc 1.100.0, uid 1000 with `CapEff: 0` (ordinary user, no elevated token). tmpfs scratch: `/tmp` `rw,noatime,inode64,huge=advise`, fsid `b6b575e7…`, mount view 74, no Btrfs tree. Btrfs scratch: `/home` on `/dev/nvme0n1p2[/@home]` `rw,noatime,compress=zstd:1,ssd,discard=async,space_cache=v2,subvolid=257,subvol=/@home`, fsid `b68b24bc…`, view 61, tree 257. All fixtures are synthetic files under a disposable parent; no game, external root or elevated operation was touched.

| Proof case | Linux Btrfs | Linux tmpfs | Linux ext4 | Hosted `windows-2025` NTFS | Power |
|---|---|---|---|---|---|
| `native_no_replace_honors_concurrent_create` | ran 2026-11-22, passed | ran 2026-11-22, passed | not run | not run | — |
| `native_replace_records_actually_displaced_file` | ran 2026-11-22, passed | ran 2026-11-22, passed | not run | not run | — |
| `native_rejects_root_and_parent_substitution` | ran 2026-11-22, passed | ran 2026-11-22, passed | not run | not run | — |
| `native_detach_preserves_unknown_file` | ran 2026-11-22, passed | ran 2026-11-22, passed | not run | not run | — |
| `native_error_does_not_prove_no_change` | ran 2026-11-22, passed | ran 2026-11-22, passed | not run | not run | — |
| `native_namespace_survives_claimed_barrier` | ran 2026-11-22, passed (process-only) | ran 2026-11-22, passed (process-only) | not run | not run | Task 8, not run |
| `library_catalog_never_names_missing_payload` | ran 2026-11-22, passed | not run (library staging unqualified on tmpfs) | not run | not run | Task 8, not run |
| `btrfs_cross_subvolume_copy_does_not_use_cross_subvolume_rename` | not run (subvolume teardown EPERM) | N/A | N/A | N/A | — |

## Per-row evidence

1. **No-replace create** — child created `Data/textures/new.dds = edit\n` at the `BeforeNativeEffect` cut; the port rechecked the parent identity/leaf and the target, then `renameat2(RENAME_NOREPLACE)` returned `EEXIST` (raw error recorded at `AfterNativeEffect`). Outcome `Conflict`; target retained `edit\n`; the private stage still held `mod\n`; sentinels unchanged.
2. **Replace with actual displacement** — child moved `winner.dds = base\n` to `old`, wrote `edit\n` at the public name and recorded its independent file identity. After the checkpoint the port observed an identity different from the frozen pre-image and refused cleanly: outcome `Conflict`, `edit\n` remained public, stage `mod\n` intact, and both independent recovery copies (`original` and `step`) held `base\n` at separate operation-owned refs. The harness accepts this refusal branch or the exact-retention branch; the refusal branch was the observed one.
3. **Root and parent substitution** — the child moved `Data/textures` outside `Data` and put a symlink in its place. Re-resolution through the live binding failed (`RESOLVE_NO_SYMLINKS`/beneath), so the port returned `Unavailable(OutsideBoundary|TargetUnavailable)` or `Conflict` before any rename. `moved-parent/winner.dds` retained `base\n`, the stage retained `mod\n`, and no outside sentinel changed. Reopening after replacing the installation with a matching-inode subvolume was refused with `TargetUnavailable`.
4. **Checked detach** — same swap race with no staged output: outcome `Conflict`, the child's `edit\n` remained at the public name, no retained slot was created, and nothing was deleted or settled as a removal.
5. **Error is not "no change"** — injection returned `Fault::LoseAcknowledgement` after a successful exchange. The port reobserved and returned `Ambiguous` with the new target `mod\n`, the canonical retained slot holding `base\n`, and the logical stage absent. Genuine missing-target and read-only-metadata (`user.r03` xattr) refusals were exercised separately and were not reported as success. No claim is made for Win32 1175/1176/1177.
6. **Namespace barrier** — process-only cuts. Before publish the target was absent; after publication and before the barrier the child process saw the new name; after the claimed Linux barrier a reopened verifier saw the same object identity and bytes. No power-loss claim.
7. **Catalog never names a missing payload** — on Btrfs the real `Catalog` used the native receipt: the interrupted stream left profile revision 0 and no reconcilable stage; the prepared cut returned the intact ID from `reconcile`; changing the payload made `publish_import` and `discard_import` return `StageChanged` and kept the changed bytes unlisted; a missing payload refused publication and preserved the held bytes; the committed cut kept the original snapshot bytes after rewriting the selected ZIP, and replaying the same ID returned the recorded snapshot without a second publication or revision increment.
8. **Btrfs cross-subvolume** — not run. `btrfs subvolume create` succeeded and `btrfs subvolume delete` returned `EPERM` ("Operation not permitted"), which the runbook treats as not run. The scratch path was retained; no shim or other filesystem was substituted.

## Commands

- `python tests/platform/m1/native-publication/run.py /tmp/opencode/r03-run` (tmpfs): exit 0; 8 passed, 1 ignored; library case reported `R03 NOT RUN: catalog native receipt unavailable on this volume`; subvolume case reported not run.
- `python tests/platform/m1/native-publication/run.py /home/parth/.cache/r03-run3` (Btrfs): exit 0; 8 passed, 1 ignored; library case ran all three real catalog cuts; subvolume case reported teardown EPERM.
- `cargo test --locked -p marduk-platform --test native_publication`: compiles and passes; without `MARDUK_R03_SCRATCH` every runtime case prints `R03 NOT RUN`.
- `cargo test --locked -p marduk-storage --test library_contract`: 5 passed (tmpfs remains unqualified).
- `cargo check --locked --target x86_64-pc-windows-msvc -p marduk-platform --tests`: passed; this is compile-only, not NTFS runtime.
- `cargo fmt --all -- --check`, `cargo build --workspace --locked`, `cargo test --workspace --locked`: passed.

The frozen `run.py` builds its probe with only `.rlib` artifact directories. In this workspace the `zeroize_derive` proc-macro artifact lives in a separate build directory, so the probe first failed with `E0463: can't find crate for marduk_storage`. Symlinking the hashed proc-macro `.so` into `target/debug` (already a search path) let the frozen script complete unchanged. The harness assertions themselves were not modified; no assertion failed.
