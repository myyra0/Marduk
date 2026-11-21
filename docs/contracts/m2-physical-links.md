# M2-D02: physical link methods and fallback

**Status:** Fallback consent selected; native contract and routes are drafts. Consumes [copy redeploy](m2-redeploy.md) and the [filesystem proof matrix](../research/filesystem-support.md). M2-D03 owns added filesystem/driver qualification.

Preserve Hardlink → Symlink → Copy as the order of recommendations when a method is **known unsupported**. Each fallback needs explicit approval for this plan, including when more than one target changes method. No remembered fallback permission. The alternative, a saved per-instance fallback, would preserve unattended convenience but was not selected. Do not rewrite the requested preference from a single approved plan. No private-to-physical fallback.

| Requested | Available only with | What Preview can offer otherwise |
|---|---|---|
| Copy | Checked, durable copy publication for that target and metadata | Block; there is no next method. |
| Hardlink | Qualified native link operation; verified mutable source and target in the same link domain, not merely the same drive; game/tool route can use it | For proven incompatible link domain or unsupported capability, propose Symlink if qualified for that route, otherwise Copy if qualified. |
| Symlink | Qualified creation privilege, exact target/source visibility, and game/tool behavior; no elevated fallback | For proven missing symlink-specific privilege or unsupported link type/route, propose qualified Copy. |

Platform classifies `Available`, `Unsupported(reason)` and `Blocked(reason)` from the verified source/target relationship. Treat unknown I/O, ordinary write or sharing denial, full disk, unsafe paths, missing/rebound source, and unqualified recovery as **blocked**, not as alternative-method signals. A method choice changing after review fails/requires new review; a failure after effects begin goes through journal recovery, never a retry with another method.

Do not link a public target to immutable mod payload, unmanaged originals, or recovery pre-images. Materialize a separate manager-owned mutable source per generation and target, in a qualified storage role, before publication. Do not share mutable sources between targets, profiles or generations. Journal the original, owned publication identity/link, source and cleanup references. Only the known installed link may be detached; do not follow an unexpected link to write or unlink its referent. Content edits through a managed link are not proof of ownership of the new bytes: use the approved recovery/capture policy, preserve changed bytes and the original, and keep evidence if ownership is uncertain. No reclamation until both active and recovery references are gone.

## Planner witnesses

| Fixture in `physical_methods` | Assertion |
|---|---|
| Hardlink across separate Btrfs subvolumes and separate Windows NTFS volumes; unprivileged symlink where relevant | Preview records requested, proposed effective method and reason. Decline and automatic deployment both make zero writes; explicit approval affects only the exact plan. Check same link domain separately as a positive case. |
| Swap source volume or change privilege after Preview; inject ENOSPC and ordinary write denial independently | No downgrade or new effect. Stale approval does not authorize Copy. A failure after publication begins leaves recoverable state. |
| Two targets with identical mod bytes; write through one deployed link | Immutable payload, second target and unmanaged original remain unchanged. Restore never deletes an unknown replacement; edit and backup survive loss/restart, or are reconciled only by the reviewed policy. |
| Kill after link publication or detachment, before ACK | Restart compares actual and expected link/source identities, not pathname or digest alone. Recover exact prior committed generation or report conflict. No link-source cleanup can strand an active generation. |

Before dispatch, freeze intent and error types, link-source placement and concrete primitives against M2-D01, and prove all four cases as applicable on ext4, Btrfs and native NTFS. `physical_methods` (proposed) plus native process/power checks are distinct from copy qualification. Probes run in owned scratch roles, not user game directories. Failed qualification blocks the affected support claim; it does not remove that route from parity.
