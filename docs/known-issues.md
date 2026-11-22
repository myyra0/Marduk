# Known issues

Open defects and unproven claims at this checkpoint. Nothing here is silent: each entry says what can go
wrong, who it affects, and what would close it. Anything fixed since the finding is not listed.

Last reviewed: 2026-11-22, at the commit recorded in `docs/research/m1-native-results.md`.

## Open defects

**A mod file changed after import can still be previewed and applied.**
`crates/storage/src/native_effects.rs` (`source`) fingerprints the live payload instead of the digest recorded in
the catalog. Change a staged payload between import and deploy and the deploy applies the changed bytes. Affects
anyone whose library files are modified outside Marduk (sync tools, another process, a bad copy). Closing it means
resolving the catalogued winner and comparing its stored length and digest before sealing and staging, with a
witness that mutates a payload after import and expects a typed refusal.

**A displaced file can stop being flagged as unresolved.**
`crates/storage/src/operation_journal.rs` (`write_readable_slot`) decides blocking from the latest observation only.
A slot that was positively reported as displaced, then briefly unreadable, then readable again, clears the unresolved
flag without operator review. Recovery can then continue past a displacement the user never saw. Closing it means
keeping the positive displacement latch separate from current slot readability, with reopen tests for the canonical
and alternate slots.

**An exchange can settle with incomplete slot evidence.**
`crates/storage/src/native_effects.rs` (`exchange_slots`) reports the stage as absent when the distinct `displaced`
slot is occupied, even if the stage is unreadable or holds an unexpected object. A matching target and displaced
fingerprint can then settle cleanly without accounting for every slot. Closing it means classifying both occupied
slots explicitly and refusing a clean settle unless all of them are positively accounted for.

**A read-only original can fail after the public write on Windows.**
`crates/platform/src/effects.rs` still reopens the retained original with `FILE_WRITE_ATTRIBUTES` after the moves. An
original that is readable for preview but denies that right fails *after* the game folder has changed. Closing it
means binding and proving timestamp-write authority before publishing, or dropping the post-move write entirely, with
a witness under an ordinary NTFS token.

**One evidence row names the wrong revision.**
`docs/research/m1-native-results.md:485` attributes the new flush sequence to `62b9b94b`, which still contained the
file flushes that were later removed. It should name the commit that carries the current sequence.

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
- **Windows desktop smoke.** The packaged desktop smoke runs on Ubuntu only; Windows has a native build and unit
  tests, not a launched-shell smoke.
