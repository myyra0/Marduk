# M1-R03: qualify native placement and durability

**Status:** Proposal and runbook, not run. This packet creates only an approved disposable proof on synthetic local volumes; it does not implement the production deployment adapter. [R02](m1-recovery.md) consumes the result, and [R04](../superpowers/plans/2026-11-20-m1-working-core.md#m1-r04--recovery-storage-and-game-smoke-checkpoint) proves the integrated application separately.

## Candidates

| Effect | Linux ext4/Btrfs candidate | Native Windows NTFS candidate |
|---|---|---|
| Create without clobber | Directory-relative `renameat2` with `RENAME_NOREPLACE` | Same-volume move with replacement and copy flags unset |
| Replace and retain actual displaced file | `RENAME_EXCHANGE` between a verified regular destination and staged replacement on the same filesystem. Prove failure handling if the target changes kind or identity; exchange itself can swap different object types | Evaluate `ReplaceFileW` with a unique same-volume backup and exact resulting states. Do not assume success preserves every required attribute or failure has no side effect |
| Remove without losing unknown bytes | No-replace move to an operation-owned, previously absent same-volume quarantine name, then classify the moved object | Same idea with a same-volume no-replace move or handle-relative equivalent. Sharing refusal is an outcome, not permission to delete differently |
| Persist bytes and namespace | Flush staged/backup data and all directory entries whose persistence matters, including both parents for moves across directories. Prove expected order separately on ext4 and Btrfs | Prove file flush and namespace behavior separately with ordinary user rights. `FlushFileBuffers` on a regular file does not by itself prove the namespace; volume flushing needs admin and is not an acceptable default |

If existing-file `ReplaceFileW` or exchange cannot meet the contract, evaluate a simpler **journaled pair of no-replace moves** (detach and retain old; then install new only if still absent) before proposing a new backend. That alternative has a visible gap: outsiders may see a temporarily absent filename. It would require a distinct intermediate persisted observation, a cut between the moves, coordinated launch exclusion, and explicit review before adoption. Do not silently replace a qualified no-gap operation with this route. No cross-volume copy-and-delete, deferred-until-reboot move, broad elevation or unconditional fallback.

References used: [Linux `renameat2` semantics and errors](https://man7.org/linux/man-pages/man2/rename.2.html), [Windows `MoveFileExW`](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw), [`ReplaceFileW`](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-replacefilew), [`FlushFileBuffers`](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-flushfilebuffers). `MOVEFILE_WRITE_THROUGH` is a candidate to measure, not justification for namespace durability; `REPLACEFILE_WRITE_THROUGH` is documented as unsupported.

## Setup and independent witnesses

- First [matrix](../research/filesystem-support.md): local ext4 and Btrfs on Linux x86-64, native NTFS on Windows x86-64, distinct isolated scratch paths. Record kernel/OS build, driver, volume and mount options, backing storage, VM version and cache/flush mode, user privilege, filesystem paths relative to a disposable root, and proof binary revision. Running on a different filesystem counts only for that filesystem.
- Create `game/Data/textures/winner.dds = base\n`, `game/Data/untouched.txt = keep\n`, and an outside sentinel `outside/sentinel = outside\n`. Use a same-volume operation-private area, not the game launcher or title files. Independently measure directory entry identities, content digests and supported metadata before and after. For library tests, keep the SQLite test database and payload tree within their named scratch volumes and record which barriers apply to each.
- Each witness is a stand-alone proof case. For lost ACK and interruption, use a child with explicit barriers and a separate verifier. For power-loss runs use a disposable VM/disk image only. Opening a pending proof must not delete a retained image or overwrite the attached scratch disk. No experiment on a real install or main device.

| Proof case | Pass condition and evidence |
|---|---|
| `native_no_replace_honors_concurrent_create` | An independent writer creates `edit\n` after preflight. A no-clobber create refuses, preserving `edit\n` and staged bytes. Must not pass because the load never reached the race cut. |
| `native_replace_records_actually_displaced_file` | Record `base\n`, change the destination to `edit\n` in the controlled race, then try replacement. It either refuses cleanly or keeps the actual displaced `edit\n` plus original recovery evidence under a typed conflict. Document which behavior the native operation can guarantee. |
| `native_rejects_root_and_parent_substitution` | Point the old path at a different root, or replace a parent with a symlink/junction to the outside sentinel. Return the right blocker and leave all outside bytes/entries unchanged. Reopen and verify recorded identity, not just string spelling. |
| `native_detach_preserves_unknown_file` | Preserve the actual external file when removing a managed name. If data was moved but could not safely be put back, report where it is; do not erase it or finalize success. |
| `native_error_does_not_prove_no_change` | Exercise applicable sharing, read-only/metadata, missing destination and filesystem-refusal paths. For each, capture exact native code plus resulting identifier/byte state for target, stage and displaced name. No claims about unobserved native codes. |
| `native_namespace_survives_claimed_barrier` | Kill after data but before namespace evidence, then after namespace evidence. Record allowed pre/post states for process and power cuts separately, and verify a genuinely new directory entry survives the claimed committed barrier. |
| `library_catalog_never_names_missing_payload` | Flush private payload and record as prepared/visible at different cut points. After reopen no committed membership may reference missing bytes; an uncommitted payload stays unlisted, not deleted by inference. |
| `btrfs_cross_subvolume_copy_does_not_use_cross_subvolume_rename` | Library and game are on separate subvolumes of one Btrfs filesystem. Stage in the game's subvolume and prove copy deploy/restore. Test eligible same-subvolume operations separately. A direct link or file rename across the boundary is not a capability of this pair; a substitute subvolume with a matching inode number cannot inherit old recovery ownership. |

## Gate output

Record the chosen Linux and Windows port and its signature, each effect's preconditions and possible observed state, supported metadata and limits, exact persistence barrier, attributable failures, and commands/results per case. Any interpretation requiring admin rights, incapable of preserving unknown bytes, or failing process/power evidence on its named target remains blocked. A narrower supported scope or lower guarantee is a user decision, not a worker fallback. An independently reviewed proof can unlock the production brief only for the cases it actually demonstrated.
