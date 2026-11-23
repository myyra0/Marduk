# ADR 0005: arbitrate installation ownership outside app-data

**Status:** Accepted, 2026-11-23. Linux runtime proof and native Windows runtime proof recorded (platform 7/7, workspace 3/3 on NTFS).

## Context

The deploy lease was a row in the operation journal, and the journal lives under the catalog
root (`$XDG_DATA_HOME/com.marduk.desktop`, `%APPDATA%\com.marduk.desktop`). A second catalog
root — a portable configuration, a different `XDG_DATA_HOME`, a second user profile on the same
machine — has its own journal and therefore its own lease. Two catalogs could each hold an
"active generation" for the same physical installation, deploy over each other, and then restore
each other's bytes. The final M1 gate reproduced it with two CLI sessions on tmpfs.

## Decision

Arbitrate ownership of the *physical installation*, not of the catalog:

- The key is a digest of the recorded physical root identity (volume plus file id/inode and birth
  witness, mount view excluded), so it is stable across reopen and distinct for sibling directories.
- The record lives outside app-data, under a per-user directory:
  `%LOCALAPPDATA%\marduk\leases` on Windows, `$XDG_STATE_HOME/marduk/leases` or
  `$HOME/.local/state/marduk/leases` on Linux.
- The record names the owning catalog root. A non-owner deploy, restore or recovery is refused with
  `ActiveDeployment` naming the owner; only the owner clears it, after a completed restore or a
  fully rolled-back deploy.
- The existing `WriterGate` serialises examine/claim/release only, on a separate `<key>.lock` object;
  the record lives in `<key>.lease`. Windows byte-range locks are mandatory, so gating the record file
  itself would make the read handle fail with `ERROR_LOCK_VIOLATION`; the two-object split keeps the
  lock off the data path on both platforms. The record outlives processes, because a deployment does.
- An unreadable or unparseable record is a typed refusal, never "absent".
- On Linux the lease directory is created mode 0700 and refused if it is not owner-only. On Windows it
  inherits `%LOCALAPPDATA%`'s ACL (SYSTEM, Administrators and the user); no narrower DACL is claimed.
  The Windows path comes from `LOCALAPPDATA`; unset or relative refuses rather than guessing.
- The journal's `root_leases` row is unchanged; this is additional arbitration, not a replacement.

The rule and its wire surface live in `docs/contracts/m1-clients.md`.

## Rejected alternatives

- **A lock file inside the installation tree.** It writes a stray file into the game folder before
  any approved effect, and a read-only or externally managed install cannot host it.
- **One global app-data catalog.** Removes portable and per-configuration catalogs, and still does
  not arbitrate two OS users.
- **Keeping the record in the journal.** It is exactly the gap this ADR closes.

## Cost if wrong

A stale or torn record refuses every catalog for that installation until the owner restores or the
operator removes the lease file by hand. Residuals carried knowingly: deployments made before this
change have no record and are invisible until their catalog claims once; a crash between the lease
claim and the journal claim leaves a record only the owner's recovery clears; the lease directory
entry is not fsynced; arbitration is per OS user, so two users share nothing.
