# M1 limitations

No open defects are recorded. This page lists what is knowingly not covered, and what is not proven.

## Accepted limitations

**A release is not fully serialized.** Clearing a lease record checks journal idleness, then clears
under the lease gate; the two steps are not one lock. Only the exact claim reference that a call
wrote can be cleared, so a stale call cannot clear a newer claim. Two simultaneous operations of the
same reference family are unproven.

**A deploy that dies between lease claim and journal claim needs manual cleanup.** The record stays
and another catalog is refused until the file `status` names is removed by hand. With an idle
journal it cannot be distinguished from a deploy still between those two steps, so it is not cleared
automatically. A `recovery:` claim in the same position is cleared by the next recovery.

**The lease directory follows the process environment.** It resolves from `LOCALAPPDATA` on Windows
and `XDG_STATE_HOME` or `HOME` on Linux. Two clients of one account launched with deliberately
different settings may resolve different directories and not see each other's record. The ownership
guarantee covers clients that resolve the same directory. Arbitration is also per OS user.

**The lease record's directory entry is not fsynced**, and on Windows the directory inherits
`%LOCALAPPDATA%`'s ACL rather than a narrower one.

**Catalog sidecars are name-resolved.** SQLite derives `catalog.sqlite3-wal`/`-shm`/`-journal` from
a pathname, so a same-privilege process can substitute them inside the verified app-data directory.
The main file and the app-data root are handle-verified; the sidecars are not. Closing it needs a
custom SQLite VFS, which is not justified for a same-privilege window.

**Final-window substitution.** A same-privilege process can substitute a parent or leaf between the
last verification and the native call. Marduk detects it afterwards, keeps the displaced bytes, and
refuses to retry automatically. It does not claim prevention.

## Unproven claims

- **Crash and power-cut recovery.** Recovery is proven across process restarts, and the interrupted
  Windows exchange is proven across a deliberate process cut. NTFS cache flushing, dirty-volume
  replay and a real power cut are not exercised, and a cut during recovery is not modelled.
- **Windows namespace barrier.** Windows runs on the narrowed guarantee in
  [ADR 0004](../adr/0004-windows-writes-use-standard-flush.md). The stricter barrier is unproven.
- **Real game smoke.** No real Skyrim SE install has been exercised; every fixture is synthetic.
- **ext4.** Excluded by decision; Linux deployment is qualified on Btrfs and tmpfs only.
- **Btrfs cross-subvolume.** The two-subvolume layout could not be torn down in the test environment.
- **Desktop write path end to end.** The installed shell is proven to launch and answer `status`.
  Import, deploy and restore use the same shared workspace and are checked statically and by the
  frontend tests against a fake transport, but no installed-shell run has driven them. A golden
  desktop-and-CLI workflow closes it.
- **Windows launched shell.** The smoke job exists and runs; on a headless hosted runner the webview
  exposes no debugging port (`DevToolsActivePort`). The job is advisory and proves nothing there.

## What would close each

The crash and power-cut experiment is specified in the
[evidence record](../research/m1-native-results.md). The Windows barrier needs a recorded experiment
under ADR 0004. The real-game, ext4 and subvolume gaps need the environment or an explicit scope
decision in `docs/PLAN.md`.
