# M1-U01: desktop and CLI contract

**Status:** Partially implemented transport/interaction contract. The shared workspace, CLI and Tauri shell exist; client-visible Deploy Preview, Deploy Apply and Restore are available where the availability record proves the composition rung (Linux Btrfs and tmpfs). Windows stays closed with `UnsupportedTarget`/`Configuration`. It covers the single M1 workflow only, not the future headless protocol or a visual layout. [Core resolution](m1-file-resolution.md), [ZIP/library](m1-zip-library.md), [native roots](m1-root-verification.md), and [recovery](m1-recovery.md) own behavior and effects.

## Shared operations, not separate engines

Both clients call the same core commands. Desktop Tauri commands decode/encode approved DTOs and call core; CLI flags map into those requests. Neither client performs its own conflict selection, scans directories to decide the winner, or edits the database. CLI accepts paths deliberately entered by its user; desktop uses a native picker and a binding or selection handle scoped to the chosen object. Don't expose generic desktop filesystem, shell, or network commands. Validate selected roots and archives again in the owning modules.

## Application data root (frozen)

One catalog location is shared by the CLI and the desktop shell. `marduk-workspace::app_data_root()` is the only resolution function; clients must not derive their own path.

- Bundle identifier: `com.marduk.desktop`.
- Root: the OS user data directory joined with the identifier.
  - Linux: `$XDG_DATA_HOME/com.marduk.desktop`, else `$HOME/.local/share/com.marduk.desktop`.
  - Windows: `%APPDATA%\com.marduk.desktop` (Roaming), matching `dirs::data_dir()`.
  - macOS: `$HOME/Library/Application Support/com.marduk.desktop`.
- The catalog and journal are `<root>/catalog.sqlite3`; earlier paths such as `.../marduk` are not read or migrated.

Phase B fixes the Tauri bundle identifier to `com.marduk.desktop`, and the desktop shell calls `marduk_workspace::app_data_root()` once and passes the result to `Workspace::open`. Tauri's own resolver is not a second authority. Environment variables are read only as the base data directory; tests isolate them, product code never adds a second override.

| Operation | CLI proposal | Request and authoritative result |
|---|---|---|
| Choose installation | `marduk setup --game-dir <dir>` | Bind the selected installation, return its stable `InstallationId` and availability; initialize or return the sole M1 profile. No game writes. |
| Read workspace | `marduk status` | Return bound target label, profile ID/revision, member IDs/order, availability and pending/active operation status. On restart load this before enabling Apply/Restore. |
| Preview and publish import | `marduk import preview <zip> --name <name> --at-revision <revision>`; `marduk import apply <import-id>` | Bind selected ZIP/label to the exact profile revision. Preview returns the durable prepared ID and content summary. Apply returns the resulting revision and snapshot; it can't silently switch profile. |
| Reorder | `marduk order --at-revision <revision> <member-ids>…` | Submit each current membership ID exactly once in intended low-to-high order. Reject duplicates, omissions and stale revisions. Never use visible row index as identity. |
| Preview and publish deployment | `marduk deploy preview --at-revision <revision>`; `marduk deploy apply <plan-id>` | Preview includes root label, selected winner/path, needed original backup and no-write blockers. Apply uses only the exact approved ID; returns an `OperationId` and status. A stale plan requires another preview. |
| Restore or read an operation | `marduk restore`; `marduk operation <operation-id>` | Restore acts on recorded deployed generation and returns a tracked outcome or `NoActiveDeployment`. Status shows pending or terminal result, recovery conflicts and available actions. Don't allow a changed configured path to retarget restore. |
| Reconcile interrupted work | `marduk recover` | Resolve the operation recorded as pending by an interrupted apply or restore and return its typed operation, or `null` when nothing is pending. Read the journal first, so a clean root answers without opening a native effect port; a resolved operation returns its recorded outcome with no second effect. Reconciliation is a real crash requirement, not a speculative endpoint: without it a desktop restart would only display the pending operation and never settle it. An unbound or unqualified root keeps its typed refusal, never a simulated success. |

Deploy and restore availability is the record's answer for the installation root's volume class: `marduk_core::qualification` requires a `NativeEffects`/`Deploy` proof at the composition rung or above. The client-visible rung is recorded after the first real run, so it is not a precondition; crash and power-cut evidence is deferred and unproven. Windows NTFS has no composition proof and stays refused. There is no environment variable, feature flag or CLI override.

Use opaque typed IDs (string DTOs, not JavaScript numbers) across the client boundary. Revisions are serialized as decimal strings so JavaScript cannot round them. Read-only snapshots carry the exact revision, not an inferred sequence. UI previews show the admitted and reviewed state; only explicit Deploy Apply or Restore can initiate new writes to the game directory. `recover` is the one exception: it may settle an already-recorded pending operation, but it must never create work. Publishing a staged import is a separate explicit Apply. The client cannot treat progress text, callback delivery, process exit, or an older replayed snapshot as current state.

Keep Rust-side transport DTOs canonical. For the small M1 command set, write TS mirrors in `app/src/transport` and check them against Rust-serialized fixtures, rather than adding a generator now. Give transport one stable code mapping from domain errors; domain modules keep their own typed errors and clients do not branch on error text. Freeze exact envelopes before dispatch. Cover a snapshot, pending/terminal status and error, including an ID/revision greater than `2^53`. Expand the same check with M2 operations; revisit generation only if maintaining the checked mirrors costs more than adding tooling. This is not the headless protocol.

M1 uses a durable scoped plan ID for preview/apply across CLI invocations (no server daemon). It persists only validated Rust-side inputs and identities; never persist a security blessing that bypasses revalidation. Retain a link from a claimed plan to its `OperationId` for replay. A cold restart or Tauri reconnection reads authoritative status; live progress can be an ordered typed Tauri Channel keyed by operation and sequence. Missing or duplicated events cause a status refresh, not a retry or completion message. When that status load reports pending work, the desktop calls `recover` once and reloads status. A typed refusal, or an accepted operation whose recorded result carries a non-null blocker, is shown as a refusal and never retried automatically; a `null` result means the journal held nothing pending, so the stale status is reloaded without claiming reconciliation. Reconciliation is reported only when the reloaded status has nothing pending. Do not expose Cancel once the operation has entered a committing/public-write phase.

Stable wire outcomes include `StalePlan`, `Busy`, `InvalidPath`, `TargetUnavailable`, `RecoveryConflict`, and `NoActiveDeployment`. Success conveys the resulting revision or operation phase; an accepted request is not necessarily a completed operation. A blocked reconciliation is an `Ok` envelope whose operation carries a blocker and exits 1, like a typed refusal; `null` and a blocker-free operation exit 0. Other stable nonzero CLI exits for input/recovery failures and a structured result option are implementation brief details to freeze with tests. Diagnostics must not include raw transient provider secrets or arbitrary OS path disclosure.

## Acceptance

| Witness | Distinguishing result |
|---|---|
| `desktop_and_cli_share_winner_and_final_state` | Drive both clients with the same fixture. Preview/apply agree on `b\n`, reorder changes the intended winner, and Restore restores `base\n`; the real engine path must be checked via CLI or installed shell, not only frontend fake. |
| `stale_preview_requires_another_review` | Preview at revision 8, reorder to revision 9 from another client, then Apply the old plan. Show `StalePlan`, no filesystem effects, new preview required. |
| `repeated_apply_and_reconnect_do_not_repeat_writes` | Interrupt event delivery and replay the same approved ID. Status returns the prior operation and authoritative generation; no duplicate file writes. |
| `recovery_conflict_does_not_display_restored` | Show external-edit conflict and safe recovery state without claiming Restore succeeded or offering a blind second write. |
| `keyboard_order_uses_member_id` | With focus in the mod list, reorder exact selected member; text input does not dispatch the pattern as a hotkey. Selection follows the ID, not a recolored or moved row. |
| `desktop_has_no_generic_write_command` | Custom commands are restricted to the trusted local window; malformed IDs and unexpected paths cannot exercise a general native write or shell capability. |
| `wire_fixtures_keep_exact_ids_and_error_codes` | Parse Rust-produced fixtures through the desktop transport. Exact large IDs/revisions round-trip as strings; the correct stable error reaches the UI even when human-readable wording changes. Check invalid DTOs are refused and do not trigger writes. |

Future baseline `cargo test -p marduk-cli --test workflow`; freeze the browser/component commands in the approved scaffold. Run installed desktop smoke on Linux and Windows. Record the filesystem separately for any actual game-write proof. No screenshot or local UI mock is native adapter evidence.
