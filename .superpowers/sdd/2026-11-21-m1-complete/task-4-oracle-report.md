# Task 4 — native root oracle freeze

| Witness | Exact public result |
|---|---|
| Missing Skyrim marker at the installation root, even with a marker in `Data` | `Err(RootError::UnsupportedTarget(UnsupportedReason::Configuration))`, not `TargetUnavailable`. Both inert marker names independently suffice. |
| Reserved `textures/NUL.dds` | `Err(PathError::InvalidComponent { index: 1, reason: PathViolation::ReservedDeviceName })` from C01, before native lookup. |
| Two case-equivalent leaf names on Linux | `Err(RootError::AmbiguousName)` from parent resolution. One equivalent object retains its existing spelling and observes as `Regular`. |
| Missing leaf / missing parent | `Ok(TargetObservation::Absent)` under verified direct `Data` / `Err(RootError::MissingParent)` during resolution. |
| Replacement directory or Btrfs snapshot at the recorded locator | `Err(RootError::TargetUnavailable)` from reopen; no adoption of a valid but different installation. |
| Linux symlink, Windows junction, or nested Btrfs subvolume traversed below `Data` | `Err(RootError::OutsideBoundary)` from resolution. |
| Regular hardlink / Linux FIFO at the leaf | `Ok(TargetObservation::Unsupported { reason: UnsupportedReason::Metadata })` / `Ok(TargetObservation::Unsupported { reason: UnsupportedReason::ObjectKind })`. Freeze § D puts these leaf refusals on the observation surface; a caller converts them to `UnsupportedTarget(reason)` when deciding whether to permit a plan. |

Namespace lists and sentinel bytes are checked before/after probes; public `RootError` debug diagnostics are checked for path disclosure. No plan/stage calls exist on this seam. Persisted owner proofs (`StageEvidence.owner` and storage's owner encoding) are compared evidence, not live handles or authority.

No new public API required. The game integration test needs test-only `marduk-platform` and `tempfile` dependencies in the future game manifest; the platform oracle uses a neutral descriptor and does not depend on games.

Both Btrfs cases are Linux-only and ignored in the default run. With `MARDUK_M1_BTRFS_SCRATCH` set to a disposable writable Btrfs parent, run `cargo test --locked -p marduk-platform --test root_contract btrfs -- --ignored`. Explicit invocation without the variable or with a failed setup is a failure, not a pass. The test creates owned children without elevation, checks snapshot inode equality plus distinct tree IDs, and requests `btrfs subvolume delete` on success and panic. The nested case needs no mounted fixture; an independently mounted child is not exercised.

Hosted `windows-2025` NTFS must exercise a real `/J` junction, between-binding replacement, single case-equivalent lookup, native volume/file identity, and hardlink refusal. It does not attempt a Windows symlink, two case aliases, FIFO, or Btrfs scenarios. Linux uses a real symlink and no junction. An ordinary-rights setup failure on the named filesystem is not an implicit skip.

Freeze baseline: Linux `7.3.5-1-cachyos`, UID 1000; regular fixtures default to `/var/tmp` (Btrfs, subvolume 261, `rw,noatime,compress=zstd:1`; `/tmp` is tmpfs). `MARDUK_M1_GAME_SCRATCH` can select another qualified test parent. Focused game command stopped at missing `marduk-games`; focused platform and workspace tests stopped at missing § D types/functions (`E0432`). Formatting and the workspace build passed; the build warned that platform does not yet use its core dependency. No witness ran locally; Btrfs gated runs (env unset), ext4, and Windows compile/run are not run.
