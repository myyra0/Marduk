# ADR 0005: one client and one catalog per installation

**Status:** Accepted.

## Context

M1 briefly supported simultaneous clients and multiple catalogs over one installation. That required a
lease with a gate, per-call claim references, compare-and-swap release and environment-resolved state.
Every review round found another window in that machinery, while the product has no requirement for
simultaneous writers.

## Decision

- **One client at a time.** The desktop and the CLI share one account-wide instance guard. A second
  client fails with a typed refusal instead of waiting. The guard is held for the desktop process or
  for the CLI command.
- **One catalog per installation, marked on the installation.** A deploy creates
  `.marduk-deployment` in the installation root before its first public effect. The file contains the
  owning catalog root and is never updated. A completed restore or a fully rolled-back deploy deletes
  it. A marker naming another catalog refuses preview, deploy, restore and recovery with
  `ActiveDeployment`; status shows the owner.
- **No coordination protocol.** There is no lease gate, no claim token, no compare-and-swap and no
  per-user ownership directory. Single-instance makes them unnecessary; the marker makes sequential
  catalog switching safe.
- **The journal stays authoritative** for what is deployed. The marker records only ownership; it
  stores no generation and is not consulted to decide what to restore.

## Rejected alternatives

- **Keep the lease and fix each window.** The windows are inherent to multi-writer support the product
  does not need; the fix list grew with every review.
- **No marker.** A second catalog would silently deploy over the first and record already-modded files
  as originals, corrupting a later restore.
- **A per-user ownership directory.** It forced key derivation and environment agreement, and still
  missed sequential catalogs when `HOME` or `XDG_STATE_HOME` differed.

## Cost if wrong

- The CLI is refused while the desktop is open. A user who wants both must close one.
- If the owning catalog is deleted while a deployment exists, the marker blocks other catalogs until it
  is removed by hand at `<installation>/.marduk-deployment`. Status names the owner, not the path.
- The marker adds one file inside the game folder for the duration of a deployment; Steam ignores it.
- A crash between writing the marker and recording the operation leaves a marker that the owning
  catalog's recovery clears when the journal is idle.
