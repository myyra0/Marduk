# M1 remaining

What is left for the M1 milestone specifically, and what is not M1 at all. Written 2026-11-23 after the
third whole-packet review blocked at `2d80c99` and its findings were fixed.

## Short answer

Every M1 deliverable exists and runs, and every defect the final review found has been fixed with a
witness or a native run behind it. What remains is **deferred evidence, a Windows lease check in
flight, and gate acceptance** — not missing features or known code defects.

## The M1 task list, and its state

| Task | Deliverable | State |
|---|---|---|
| 1 | Preflight, dependency and native freeze | done |
| 2 | ZIP admission (`marduk-archive`) | done |
| 3 | SQLite catalog, revisions, replay, reconciliation | done |
| 4 | Skyrim SE layout, root authority, Linux and Windows adapters | done |
| 5 | Native effect outcomes and durable library staging | done on Linux Btrfs/tmpfs and Windows NTFS |
| 6 | Operation journal, copy adapter, deterministic fault tests | done |
| 7 | CLI and Tauri desktop for the same operations | done; shell launches and answers commands on Linux, Windows native build green |
| 8 | One final SHA, native results, remaining gates, independent review | **open**: re-review of the post-gate fixes |

## Fixed after the final review

| Finding | Fix | Evidence |
|---|---|---|
| Frozen library oracle drifted from its freeze commit | `6223d27` restores `57de606` byte-for-byte and moves the two witnesses to an unfrozen module | `git diff 57de606` empty; 10 frozen tests pass |
| Two catalogs could each own one installation | `5d10180` installation lease outside app-data; `3bd111a` gates on a separate lock object so Windows mandatory locks cannot block the record | workspace/platform witnesses on tmpfs and Btrfs; native NTFS 7/7 platform, 3/3 workspace; CLI end-to-end |
| NTFS re-applied a vacated name's creation time to the next publish, blocking a deploy after a restore | `196a477` binds and restores the staged object's birth time around the move, as `replace` already did | witness failed on NTFS before the fix and passes after; platform 15/15 on the VM |
| Recovery rejected the state between the two Windows renames | `3f58e98` reverts a pending deploy and completes a pending restore | native NTFS process cut: Deploy `rolled_back`, Restore `completed` |
| Displaced bytes hidden in the physical stage slot | `e5c584c` reports `slot: "stage"` and latches until review | tmpfs and Btrfs witnesses |
| Abandoned imports never reclaimed, `reconcile` never called | `750d9d8` ownership before staging; `f406c1c` one startup reconcile in status | storage witnesses both roots; desktop and CLI show the report |
| Privileged CI mount and unguarded Windows delete | `2982621` `/dev/shm` asserted tmpfs; Windows cleanup guarded by CI and profile | hosted job pending |
| Desktop Apply/Restore discarded progress | `d32575d` engine transitions streamed to the channel | core/workspace/app witnesses; real desktop run pending |
| Moved installation reported two different codes | `6e214f7` identity check before volume classification | witness on status and preview |
| README claimed a Windows launched shell | `2982621` qualifies the claim | text |

## Open or unproven

| Item | Size | Blocks "M1 done"? |
|---|---|---|
| Hosted CI asserting `/dev/shm` is tmpfs | one CI run | no |
| Crash and power-cut recovery | large (lab protocol) | yes — M1's acceptance names recovery tests |
| Windows namespace barrier experiment | medium | no, if the narrowed guarantee in ADR 0004 stands |
| Real Skyrim SE run | medium (needs the game) | only if M1 is claimed as real-world verified |
| ext4 qualification | medium (excluded by decision) | no, if declared out of scope |
| Btrfs cross-subvolume | medium (blocked by teardown) | no, if declared out of scope |
| Windows launched-shell smoke | environment-bound | no, but a named evidence gap |
| Whole-packet re-review | a review | yes — it is the acceptance itself |

## Not M1 at all

- onboarding and settings, complete P0 workflows (M2)
- downloads, mod sources, archives beyond ZIP, more games (M3)
- private views per platform, retained P2 features, broader compatibility (M4)
- launching the game, process management (M2)

## To close M1

1. Finish the Windows lease run and the hosted CI run.
2. Either run the crash/power-cut protocol or record in `docs/PLAN.md` that M1 is accepted without it,
   naming the risk that carries.
3. Run one real Skyrim SE install, or record the same kind of exclusion.
4. Have the gate accept that definition.
