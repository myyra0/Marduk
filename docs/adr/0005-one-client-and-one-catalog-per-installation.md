# ADR 0005: one client and one catalog per installation

**Status:** Accepted; scope revised 2026-11-23.

## Context

M1 briefly supported simultaneous clients and multiple catalogs over one installation. That required a
lease with a gate, per-call claim references, compare-and-swap release and environment-resolved state.
Every review round found another window in that machinery, while the product has no requirement for
simultaneous writers.

## Decision

- **One client per catalog.** The desktop and CLI hold the catalog's `instance.lock` for the desktop
  process or CLI command. Competing clients on that catalog get a typed refusal without waiting.
  Once the catalog is resolved, the lock location does not consult the environment.
- **One catalog per installation.** The verified installation binding publishes a marker with a
  no-replace move before deployment can affect the installation. Catalogs can run concurrently;
  the marker, not a cross-catalog lease or protocol, arbitrates ownership.
- **The journal stays authoritative** for what is deployed. The marker records only ownership; it
  stores no generation. See the [client contract](../contracts/m1-clients.md) for refusals and cleanup.

## Rejected alternatives

- **Keep the lease and fix each window.** The windows are inherent to multi-writer support the product
  does not need; the fix list grew with every review.
- **No marker.** A second catalog would silently deploy over the first and record already-modded files
  as originals, corrupting a later restore.
- **A per-user ownership directory.** It forced key derivation and environment agreement, and still
  missed sequential catalogs when `HOME` or `XDG_STATE_HOME` differed.

## Cost if wrong

- The CLI is refused while the desktop is using the same catalog. Clients on distinct catalogs
  can still use separate installations.
- If the owning catalog is deleted while a deployment exists, the marker blocks other catalogs until it
  is removed by hand at `<installation>/.marduk-deployment`. Status names the owner, not the path.
- A process cut before publication can leave a uniquely named temporary in the installation root;
  it does not claim the installation. Windows standard flushing does not prove power-cut durability
  ([ADR 0004](0004-windows-writes-use-standard-flush.md)).
- Pre-redesign unframed records are not distinguishable from truncated writes. They fail closed;
  restore with the previous build before upgrading a still-deployed development installation.
