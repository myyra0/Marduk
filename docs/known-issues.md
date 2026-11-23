# Known issues

Open defects and unproven claims at this checkpoint. Nothing here is silent: each entry says what can go
wrong, who it affects, and what would close it. Anything fixed since the finding is not listed.

Last reviewed: 2026-11-23, at the commit recorded in `docs/research/m1-native-results.md`.

## Open defects

None recorded.

## Known residual, documented rather than fixed

**Catalog sidecars are name-resolved.** SQLite derives `catalog.sqlite3-wal`/`-shm`/`-journal` from a pathname, so a
same-privilege process can substitute them inside the verified app-data directory. The main file and the app-data
root are handle-verified and re-proved after the open; the sidecars are not. This is stated in
`docs/contracts/m1-recovery.md` ("Catalog file containment") and matches the containment ruling of 2026-11-22.
Pinning them needs a custom SQLite VFS, which is not justified for a same-privilege window.

**Final-window substitution during a deploy.** A same-privilege process can substitute a parent or leaf between the
last verification and the native syscall. Marduk detects it afterwards, retains actually displaced bytes, and refuses
to retry automatically. It does not claim prevention. See the contract's "Known limitation" paragraph.

**Cross-catalog installation ownership.** Deployments made before the installation lease existed carry no record, so
another catalog root can deploy over them until their owning catalog claims once. A crash between the lease claim and
the journal claim leaves a record only that owner's recovery clears; it is never adopted silently. The lease directory
entry is not fsynced, and arbitration is per OS user — two users on one machine share nothing. See
[ADR 0005](adr/0005-arbitrate-installation-ownership-outside-app-data.md).

## Unproven claims (no defect, no evidence)

- **Crash and power-cut recovery.** Recovery is proven across process restarts, and the interrupted-exchange shape is
  proven across a deliberate process cut on NTFS. NTFS cache flushing, metadata journal replay, dirty-volume recovery
  and a real power cut are not exercised, and a cut during recovery is not modelled. The experiment specified in
  `docs/research/m1-native-results.md` has not been run to a conclusive result.
- **Windows namespace barrier.** Windows deploy uses the narrowed guarantee in `docs/adr/0004-windows-writes-use-standard-flush.md`.
  The stricter barrier is "theoretically approved for testing, empirical proof deferred".
- **Real game smoke.** No real Skyrim SE install has been exercised; all fixtures are synthetic.
- **ext4.** Excluded by decision. Linux deployment is qualified on Btrfs and tmpfs only.
- **Btrfs cross-subvolume (`EXDEV`).** The subvolume case could not be torn down in the test environment, so the
  two-subvolume layout is `not run`.
- **Windows desktop smoke.** The packaged desktop smoke now has a Windows job, but it fails at session creation
  (`session not created: DevToolsActivePort file doesn't exist`): `msedgedriver` starts and the app never exposes a
  debugging port, consistent with a runner session that has no interactive desktop for WebView2. The job is advisory.
  Unproven until it runs somewhere with a real desktop session — the Windows VM is the candidate. Recorded rather than
  hidden by dropping Windows from the matrix.
