# M1 limitations

No open defects are recorded. This page lists what is knowingly not covered, and what is not proven.

## Accepted limitations

**Ownership and marker durability.** See [ADR 0005](../adr/0005-one-client-and-one-catalog-per-installation.md)
for catalog and installation ownership, upgrade limitations, and marker durability.

**Catalog sidecars are name-resolved.** SQLite derives `catalog.sqlite3-wal`/`-shm`/`-journal` from
a pathname, so a same-privilege process can substitute them inside the verified app-data directory.
The main file and the app-data root are handle-verified; the sidecars are not. Closing it needs a
custom SQLite VFS, which is not justified for a same-privilege window.

**Final-window substitution.** A same-privilege process can substitute a parent or leaf between the
last verification and the native call. Marduk detects it afterwards, keeps the displaced bytes, and
refuses to retry automatically for deployment file effects. It does not claim prevention. Marker
operations pin the root on Linux; Windows marker calls use its verified final pathname. Neither
platform excludes deliberate marker-leaf replacement between inspection and use.

### Linux remount lockout

An unmount/remount or reboot that changes the recorded mount-view ID makes the
installation unavailable, even when the same filesystem objects are present.
Restore and Recover cannot reopen that view; Setup refuses to switch views
while work is pending (`Busy`) or a generation is active (`ActiveDeployment`).
If the recorded view is still accessible at its recorded path, return to it
and finish Recover or Restore there. If it is gone, preserve the catalog and
retained originals and recover the installation manually from a known-clean
backup before setting it up afresh. Do not clear the marker or discard the
catalog to bypass a refused Restore. When no work is pending or active, rerun
Setup for the current view.

## Unproven claims

- **Crash and power-cut recovery.** Recovery is proven across process restarts, and the interrupted
  Windows exchange is proven across a deliberate process cut. NTFS cache flushing, dirty-volume
  replay and a real power cut are not exercised. Recovery cuts other than the witnessed boundaries
  are unproven.
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
  exposes no debugging port (`DevToolsActivePort`). The job is advisory: its failure is visible but
  does not fail the run, and it proves nothing there.

## What would close each

The crash and power-cut experiment is specified in the
[evidence record](../research/m1-native-results.md). The Windows barrier needs a recorded experiment
under ADR 0004. The real-game, ext4 and subvolume gaps need the environment or an explicit scope
decision in `docs/PLAN.md`.
