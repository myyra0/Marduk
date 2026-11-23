# M1 status

Every M1 task is delivered. What remains is evidence that was deferred by decision, and the
independent acceptance review.

## Task list

| Task | Deliverable | State |
|---|---|---|
| 1 | Preflight, dependency and native freeze | done |
| 2 | ZIP admission | done |
| 3 | SQLite catalog, revisions, replay, reconciliation | done |
| 4 | Skyrim SE layout, root authority, Linux and Windows adapters | done |
| 5 | Native effect outcomes and durable library staging | done; Linux tmpfs/Btrfs and Windows NTFS |
| 6 | Operation journal, copy adapter, fault tests | done |
| 7 | CLI and desktop shell over one typed API | done |
| 8 | Final SHA, evidence, remaining gates, independent review | **open: review** |

Nothing in the task list is left to implement.

## What is still open

| Item | Kind | Blocks "M1 done"? |
|---|---|---|
| Independent Sol whole-packet review | acceptance | yes — it is the acceptance step |
| Crash and power-cut recovery | deferred evidence | yes — M1's acceptance names recovery |
| Real Skyrim SE install | deferred evidence | only if M1 is claimed as real-world verified |
| Windows namespace barrier experiment | deferred evidence | no — the narrowed guarantee stands |
| ext4 | excluded | no, if declared out of scope |
| Btrfs cross-subvolume | not run | no, if declared out of scope |
| Windows launched-shell smoke | environment-bound | no, but it is a named gap |
| Desktop import/deploy/restore end to end | accepted gap | no — recorded in limitations |

## Next actions

1. Run the independent Sol review of the final commit.
2. Either run the crash/power-cut protocol or record in `docs/PLAN.md` that M1 is accepted without
   it, naming the risk that carries.
3. Run one real Skyrim SE install, or record the same kind of exclusion.

Not M1, and tracked elsewhere:

- onboarding, settings, complete P0 workflows (M2)
- downloads, mod sources, archives beyond ZIP, more games (M3)
- private views, retained P2 features, broader compatibility (M4)
- launching the game, process management (M2)
