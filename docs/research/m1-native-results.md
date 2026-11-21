# M1-R03 native results

No runtime evidence recorded. This table separates unavailable environments, blocked contracts, process witnesses and Task 8 power evidence. `not run` is never a pass.

| Proof case | Linux Btrfs | Linux tmpfs | Linux ext4 | Hosted `windows-2025` NTFS | Power |
|---|---|---|---|---|---|
| `native_no_replace_honors_concurrent_create` | not run | not run | not run | not run | — |
| `native_replace_records_actually_displaced_file` | not run | not run | not run | not run | — |
| `native_rejects_root_and_parent_substitution` | not run | not run | not run | not run | — |
| `native_detach_preserves_unknown_file` | not run | not run | not run | not run | — |
| `native_error_does_not_prove_no_change` | not run | not run | not run | not run | — |
| `native_namespace_survives_claimed_barrier` | not run | not run | not run | not run | Task 8, not run |
| `library_catalog_never_names_missing_payload` | not run | not run | not run | not run | Task 8, not run |
| `btrfs_cross_subvolume_copy_does_not_use_cross_subvolume_rename` | not run | N/A | N/A | N/A | — |

For each actual run record UTC date, proof commit, exact commands/exit, raw transcript, host/runner image, OS/kernel/driver, architecture, volume and mount options/serial/tree/view, storage/cache mode, ordinary privilege evidence, relative fixture names and subvolume teardown result. Per row include native primitive and flags, exact code if any, independent identities/digests/scoped metadata, changed and unchanged names, data/namespace barriers, child cut, blocker and review reference. Compile success is not NTFS runtime; process termination is not power interruption. Do not put outcomes from other configurations into these columns.
