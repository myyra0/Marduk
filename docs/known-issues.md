# Known issues

Open defects and unproven claims at this checkpoint. Nothing here is silent: each entry says what can go
wrong, who it affects, and what would close it. Anything fixed since the finding is not listed.

Last reviewed: 2026-11-22, at the commit recorded in `docs/research/m1-native-results.md`.

## Open defects

**An exchange can settle with incomplete slot evidence.**
`crates/storage/src/native_effects.rs` (`exchange_slots`) reports the stage as absent when the distinct `displaced`
slot is occupied, even if the stage is unreadable or holds an unexpected object. A matching target and displaced
fingerprint can then settle cleanly without accounting for every slot. Closing it means classifying both occupied
slots explicitly and refusing a clean settle unless all of them are positively accounted for.

## Known residual, documented rather than fixed

**Catalog sidecars are name-resolved.** SQLite derives `catalog.sqlite3-wal`/`-shm`/`-journal` from a pathname, so a
same-privilege process can substitute them inside the verified app-data directory. The main file and the app-data
root are handle-verified and re-proved after the open; the sidecars are not. This is stated in
`docs/contracts/m1-recovery.md` ("Catalog file containment") and matches the containment ruling of 2026-11-22.
Pinning them needs a custom SQLite VFS, which is not justified for a same-privilege window.

**Final-window substitution during a deploy.** A same-privilege process can substitute a parent or leaf between the
last verification and the native syscall. Marduk detects it afterwards, retains actually displaced bytes, and refuses
to retry automatically. It does not claim prevention. See the contract's "Known limitation" paragraph.

## Unproven claims (no defect, no evidence)

- **Crash and power-cut recovery.** Recovery is proven across process restarts, not across a crash or power cut. The
  experiment specified in `docs/research/m1-native-results.md` has not been run to a conclusive result.
- **Windows namespace barrier.** Windows deploy uses the narrowed guarantee in `docs/adr/0004-windows-writes-use-standard-flush.md`.
  The stricter barrier is "theoretically approved for testing, empirical proof deferred".
- **Real game smoke.** No real Skyrim SE install has been exercised; all fixtures are synthetic.
- **ext4.** Excluded by decision. Linux deployment is qualified on Btrfs and tmpfs only.
- **Btrfs cross-subvolume (`EXDEV`).** The subvolume case could not be torn down in the test environment, so the
  two-subvolume layout is `not run`.
- **Windows desktop smoke.** The packaged desktop smoke now has a Windows job, but the job is advisory and has not
  been green yet. Windows launched-shell behaviour stays unproven until that hosted job passes.
