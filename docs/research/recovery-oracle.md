# Recovery proof oracle

**Status:** Planner-authored acceptance design, not executed. Applies to the initial regular-file copy mode; POSIX and NTFS primitives must satisfy the same outcomes. See [architecture](../architecture.md#persistence-and-recovery), [ADR 0002](../adr/0002-journal-external-effects-before-publication.md) and [M1-R02/R04](../superpowers/plans/2026-11-20-m1-working-core.md#session-packets).

## Distinct recovery records

- **Original pre-image:** the unmanaged bytes and promised metadata before the first real deployment touched a location, or an explicit “absent.” Retain it across redeploys while that deployment lineage exists. Restore is defined against this, not against the immediately preceding mod.
- **Step pre-image:** the state just before the current operation changes this location, possibly a previously deployed file. Use it to roll back an unfinished redeploy and return to the previous committed profile generation. It may refer to an original backup or another independently recoverable artifact; it is not safe if the game can mutate it through the same file alias.
- **Actually displaced file:** the file the native operation moved out of the public name. If it differs from the approved step pre-image, preserve it and surface a conflict; an earlier snapshot does not stand in for someone else's latest edit.

## Golden fixture

Use two explicit real files at the target root: `Data/textures/winner.dds = base\n` and `Data/untouched.txt = keep\n`. First deploy publishes `winner.dds = b\n` and creates `Data/textures/a-only.dds = only-a\n`. `Data/outside-plan.txt = user\n`, when present, is never managed. The second committed deployment publishes `winner.dds = a\n`, keeping the original `base\n` recoverable. A later external modification writes `edit\n`. For M1 use the first deploy, explicit restore, and external edit cases; exercise redeploy in M2.

An operation's success is asserted independently from file contents: exact operation state, resulting active deployed generation (or none), current bytes and supported metadata, retained or released recovery copies, and a second recovery run with no additional mutation. A detached/replaced/mismatched root counts as unavailable, not as “absent file.”

| Injection / user action | Expected after subsequent recovery |
|---|---|
| Cancel or fail before a durable intent or public mutation | Bytes and previous deployment unchanged. Any orphan stage is collectible, not considered deployed. |
| Lose process after persisting first-deploy intent, before any publication | `winner = base`, `a-only` absent, `untouched = keep`; no successful generation. |
| Lose process after publishing winner but before acknowledging the step | Recover `winner = base`, `a-only` absent or rolled back if it had also been published; no successful generation. |
| Lose power between a file write and a required durability barrier | After boot the model may present an allowed old/new state per the tested native contract, but rollback restores `base` if the operation is uncommitted. Any unknown/torn output or missing claimed-durable pre-image is not success. |
| Lose process after all effects are proven durable and committed, before reply | `winner = b`, `a-only = only-a`, committed generation is active. Reconnect shows the result instead of repeating the request. |
| Interrupt explicit restore after restoring winner but before removing a managed created file | Resume restore: `winner = base`, `a-only` absent, no active generation, `untouched = keep`. Repeating Restore changes nothing. |
| External writer replaces the expected deployed file with `edit\n` before recovery | No overwrite; `edit` stays and `base` remains recoverable, with `RecoveryConflict`. Handle external creation at a target previously absent the same way. |
| An edit wins concurrently at publication | No claim of clean commit. Retain the bytes actually displaced if the primitive replaced a file; either safely put them back with a guarded operation or report their recoverable location. Preserve a still newer observed file in place. |
| Target drive disappears or root resolves to a different installation | `TargetUnavailable`; retain metadata and surviving copies, don't search for another convenient target. Check identity again on retry. |
| First deploy to B committed; second deploy to A interrupted | Restore the pre-step B bytes and keep generation B active. Preserve original `base`; a subsequent explicit Restore returns it. |
| B→A is fully committed, then Restore | Restore original `base` (not B), remove unchanged managed-created files, leave `outside-plan = user` if present. |

## Evidence separation

1. Deterministic tests use a fake that separates visible from durable changes. Test lost operations and storage reorder only where the proposed platform contract permits them; corrupt or missing durable data yields a visible failure, not invented defaults.
2. Real process-cut tests inject a stop from a separate child before/after each marked boundary and reopen the same data/target paths. Verify the files with an independent reader and use offset/size/digest checks for larger new and original bytes.
3. VM power-off tests use recorded filesystem, disk/cache/flush settings, actual reopened images and archived logs. Do not conflate a VM power cut with loss of power to a real device. Use only approved isolated scratch media. A platform guarantee stays open if its required evidence is missing.
