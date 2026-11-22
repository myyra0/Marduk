# M1 remaining

What is left for the M1 milestone specifically, and what is not M1 at all. Written 2026-11-22 at checkpoint
`c13e783`.

## Short answer

Every M1 deliverable exists and runs. What remains is **one defect, one missing smoke test, evidence that the
user deferred, and gate acceptance** — not missing features.

If the two defects below are fixed, and the deferred evidence is either produced or declared out of scope in
`docs/PLAN.md`, then nothing remains in the M1 task list.

## The M1 task list, and its state

| Task | Deliverable | State |
|---|---|---|
| 1 | Preflight, dependency and native freeze | done |
| 2 | ZIP admission (`marduk-archive`) | done |
| 3 | SQLite catalog, revisions, replay, reconciliation | done |
| 4 | Skyrim SE layout, root authority, Linux and Windows adapters | done |
| 5 | Native effect outcomes and durable library staging | done on Linux Btrfs/tmpfs and Windows NTFS |
| 6 | Operation journal, copy adapter, deterministic fault tests | done |
| 7 | CLI and Tauri desktop for the same operations | done; shell launches and answers commands |
| 8 | One final SHA, native results, remaining gates, independent review | **open**: the gate has not accepted |

## Open items

| Item | Size | Blocks "M1 done"? |
|---|---|---|
| No launched-shell smoke on Windows | small, environment-bound | no — but it is a named evidence gap: the job exists and fails at session creation on hosted runners (`DevToolsActivePort`), so WebView2 never attaches without a desktop session |
| Crash and power-cut recovery not proven | large (lab protocol) | yes — M1's acceptance names recovery tests |
| No real Skyrim SE run | medium (needs the game) | only if M1 is claimed as real-world verified |
| ext4 not qualified | medium (excluded by decision) | no, if declared out of scope |
| Btrfs cross-subvolume not run | medium (blocked by teardown) | no, if declared out of scope |
| Whole-packet gate not accepted | small (a review) | yes — it is the acceptance itself |

## Not M1 at all

These belong to later stages and are not gaps in M1:

- onboarding and settings, complete P0 workflows (M2)
- downloads, mod sources, archives beyond ZIP, more games (M3)
- private views per platform, retained P2 features, broader compatibility (M4)
- launching the game, process management (M2)

## To close M1 properly

1. Fix the exchange-slot defect.
2. Add the Windows smoke.
3. Either run the crash/power-cut protocol or record in `docs/PLAN.md` that M1 is accepted without it, naming
   the risk that carries.
4. Run one real Skyrim SE install, or record the same kind of exclusion.
5. Have the gate accept that definition.

Steps 1–2 are code. Steps 3–5 are decisions and evidence, not features.
