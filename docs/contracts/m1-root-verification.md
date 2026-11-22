# M1-R01: verify the configured root and resolve file intents

**Status:** Proposed platform interface and acceptance design. R01 proves identity and bounded lookup; it does not approve any write primitive or persistence guarantee. [R03](m1-native-proof.md) qualifies effects and [R02](m1-recovery.md) consumes their results.

## Interface

`crates/core/src/root_identity.rs` owns the portable `RootIdentity` proof-token type; it does not construct native identity or open a path. `crates/platform/src/lib.rs` owns live root handles and bounded lookup; `linux.rs` and `windows.rs` construct/verify native identity behind that interface. The adapter depends on core, never the reverse. C01 remains responsible for validating the logical `RelativePath`.

| Operation | Contract |
|---|---|
| `bind_installation(selection)` | Open a user-selected installation directory and its direct `Data` directory. Record separate installation/Data proof identities for future use; validate the selected first-game marker and return a typed description. This is not permission to create or deploy. |
| `reopen_installation(location, expected)` | Open both recorded directories again, verifying their relationship and exact identities. On missing, inaccessible, moved to another filesystem, or substituted objects, return `TargetUnavailable` and leave the already-bound record intact. Never discover a replacement automatically. |
| `resolve_existing_parent(binding, relative)` | Yield an opaque bounded parent plus the single target name, or a typed refusal. Under that parent there can be zero or one target matching the M1 logical case rule. Existing spelling is retained; if absent use the planned spelling. Multiple aliases, missing parent, dangerous object types, or unsafe boundaries are refusal states. |
| `observe_target(resolved)` | Return `Absent`, `Regular { fingerprint }`, `Unsupported { reason }`, or a path/IO error. `Absent` means the already-authorized parent has no matching target, never “could not open or verify the parent.” Observation alone cannot authorize later replacement or deletion. |

Represent separately: `InstallationBinding` (platform-owned live directory authority), `RootIdentity` (core-owned persistable proof), `ResolvedTarget` (platform-owned parent plus leaf spelling, not reusable authority after reopen), and `TargetObservation` (adapter observation, not a path). Core retry decisions consume portable outcomes from its adapter interface; callers cannot mint directory authority from a token. Public status details should be stable and redacted; don't expose unrelated mount or user configuration paths. Required refusals include `TargetUnavailable`, `MissingParent`, `AmbiguousName`, `OutsideBoundary`, `UnsupportedTarget`, and `Io`. Freeze final Rust constructors and native identity fields after the adapter proof; workers must not marshal platform handles across the API.

## Identity and path rules

Linux root tokens include the mount view ID reported by `statx` (`STATX_MNT_ID`) as well as filesystem and object identity. A remount that changes this ID returns `TargetUnavailable` on reopen and requires explicit rebind. The kernel can recycle mount IDs; the ID alone is not durable or globally unique, and equality still requires every other identity field.

- Linux ext4 and Btrfs share the bounded path implementation. Scope opaque equality to the verified filesystem/object and include subvolume identity on Btrfs; do not accept matching inode numbers in different subvolumes. In Windows NTFS qualify volume and native file identity, not a drive letter or a path-derived string. Plan IDs still bind exact preview inputs; a stable root token is not an assertion that all files are unchanged.
- Resolve each component under the approved root without following unexpected symlinks, junctions, or reparse objects. Reject crossing an unapproved nested mount/subvolume boundary. A separately approved game root may itself live on a subvolume. The *library* need not reside in the same domain: final staging is scoped to the resolved target's rename domain as defined in the [filesystem matrix](../research/filesystem-support.md).
- Treat both logical aliases and native object identity as checks. Two differently cased existing files can coexist on one Linux target but cannot be silently made one Skyrim resource. The path `Textures` must not turn into a second directory beside an existing unambiguous `textures`. Do not re-use an unrelated existing directory as the writable stage.
- Pin parent/leaf authority at the actual native operation. A lexical `starts_with`, checking a path then using an unguarded absolute write, or validating the final component without considering ancestors is insufficient. R03 must test substitutions including racing reparse/rename, record the precise limits of protection, and refuse an unsafe configuration. Root changes after a lease is acquired never imply permission to follow the new path.
- Only regular files under existing destination parents enter M1. Reject unsupported original metadata before modification, including cases the adapter cannot preserve or prove. Unexpected hardlinks, reparse metadata, multiple namespace aliases or sharing failures need explicit handling or refusal, not a silent metadata downgrade.

## Planner witnesses

| Test | Acceptance |
|---|---|
| `case_alias_and_reserved_entries_do_not_publish` | Existing `Textures` resolves the sole equivalent `textures`; if two aliases exist on a filesystem that can create them, the lookup refuses. Reserved input path fails at C01; no native write or state update. |
| `replaced_root_is_not_a_retry_target` | Same recorded path now names a new directory/volume; reopen returns `TargetUnavailable`, not `Absent`; outside and replacement sentinels unchanged. |
| `symlink_or_junction_component_is_refused` | An otherwise valid target traverses a link into `outside/`; both Linux symlink and Windows junction cases refuse this path. Exercise actual native mechanism where supported and record skipped cases. |
| `existing_original_with_unsupported_metadata_is_refused` | Unsupported object or unpreservable metadata is reported before creating a game stage file or signing off the plan. |
| `btrfs_reopened_snapshot_is_not_original_root` | **Not run.** Under Btrfs a different subvolume/snapshot may keep the same inode numbers. A recorded root cannot be reopened against that substitute; the separate library/game subvolume arrangement with the original bound root remains valid. |

Future command: `cargo test -p marduk-platform --test root_contract`. Record kernel/OS, actual filesystem/driver, mount/subvolume layout, and exact skips. A pass never substitutes for tested native publication or power-loss recovery. The Btrfs snapshot row above has no recorded run: it stays `not run` and is not a pass.

## M1 metadata scope (approved 2026-11-22)

Root discovery, read-only validation and binding establish identity and layout, not write eligibility. Write eligibility is per affected original and per qualified native filesystem/operation. Apply and Restore require fresh bound-object evidence before mutation; a failed, partial or ambiguous required probe, an unsupported object, or detected metadata outside the allowlist refuses the operation before plan approval or game staging. A later discrepancy stops the operation, preserves recovery evidence and reports conflict; it is not permission to fall back.

Admitted Linux originals are regular single-link files; Restore verifies original bytes, mode, UID, GID and mtime, qualified inode flags, and successful enumeration with no visible xattrs (including POSIX ACL/security xattrs). Admitted Windows originals are single-link ordinary files; Restore verifies original bytes, owner SID, primary group SID, the DACL's effective entries, allowed file attributes and last-write time, requires no EA or named data stream, and requires no explicit mandatory-label, resource-attribute or scoped-policy ACE on the respective successful `READ_CONTROL` queries. The Windows security digest is semantic: descriptor layout, padding, and descriptor control and provenance flags are not hashed, so two descriptors that grant the same access compare equal; the DACL is hashed as an ordered list of effective entries, so ACE order is preserved.

Linux `trusted.*` and other metadata hidden from the required unprivileged probes, and Windows privileged audit/full-SACL contents not returned by the selected queries, are unobserved and outside the verified Restore guarantee. An empty result proves only what the particular probe exposes. Do not enable privileges or claim full metadata equivalence. On any approved replacement or removal, retain the actual displaced original in operation-owned storage on the same qualified filesystem boundary, with no copy/delete fallback. Verify supported metadata and identity through publish, recovery and Restore; never discard the retained original on conflict or a failed/unverified Restore. Report scoped verification or retained-but-unrestored status explicitly. This approval applies only to qualified synthetic targets until native proof, not to real game writes.
