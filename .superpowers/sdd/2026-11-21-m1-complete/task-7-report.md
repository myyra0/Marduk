# Task 7 (phase A) report — workspace service and CLI

Date: 2026-11-22
Base: `8f0a18f`

## Delivered

- `crates/workspace` (`marduk-workspace`): one `Workspace` over `marduk_storage::Catalog`,
  `marduk_storage::operation_journal::OperationJournal` and the `marduk-platform` binding surface.
  Public `Request`/`Envelope`, stable `Code` strings, permitted `Detail` shapes and the frozen
  client DTOs. IDs and revisions are canonical decimal `String`s on the wire.
- `crates/cli` (`marduk` bin): `setup --game-dir`, `status`, `import preview/apply`,
  `order --at-revision`, `deploy preview/apply`, `restore`, `operation`, global `--json`.
  Exit `0` accepted/normal, `1` rejected/blocked/failed, `2` usage/parse/ID syntax.
  `--json` writes one envelope line to stdout and no progress prose.
- `crates/storage`: `OperationJournal::selected_installation` / `SelectedInstallation`, the
  restart read for the persisted verified pair plus the lease's pending operation and active
  generation. It reports recorded work only; it never resumes or retries.
- Root workspace members and `Cargo.lock` updated for the two crates, `serde`, `serde_json`
  and `clap = 4.6.9`.

## Behavior

- `setup` binds through `marduk_platform::bind_installation` and persists the verified
  installation/data pair with `select_verified_installation`. The returned status is read
  back from authoritative storage. No write authority is granted.
- `status` reloads the selected installation, reopens and verifies the exact pair, and reports
  the bound label, profile snapshot/revision, member IDs in low-to-high order, availability and
  pending/active state. A pending operation is displayed, never retried.
- `order` validates canonical IDs, submits the exact membership permutation and relies on the
  catalog's single revision increment. Duplicates, omissions and stale revisions map to
  `InvalidOrder`/`StalePlan`; malformed IDs map to `InvalidRequest`.
- `operation` reads journal status only. Unknown IDs return `UnknownOperation`.
- No generic filesystem, shell, network or client-supplied app-data field exists on the wire.
  The CLI derives app data from `XDG_DATA_HOME`/`HOME` (Linux) or `LOCALAPPDATA` (Windows).

## Typed unavailable routes

| Route | Result |
|---|---|
| `import preview`, `import apply` | `StageUnavailable` (`Catalog::open` keeps the native stage disconnected) |
| `deploy preview`, `deploy apply` | `UnsupportedTarget` with `detail.reason = "Configuration"` (no qualified native effect/durability port) |
| `restore` | `UnsupportedTarget` with `detail.reason = "Configuration"` |
| status `availability.import` | `StageUnavailable` |
| status `availability.deploy` / `restore` | `UnsupportedTarget`, `Configuration` |

No deploy, restore or import success is claimed or simulated.

## Verification

Commands run locally (Linux x86_64, Rust/Cargo `1.100.0`):

```text
cargo fmt --all -- --check                                             PASS
cargo build --workspace --locked                                       PASS
cargo test --locked -p marduk-workspace --test workflow                PASS (9/9)
cargo test --locked -p marduk-cli --test workflow                      PASS (4/4)
cargo test --workspace --locked                                        PASS (no failures; pre-existing ignored tests unchanged)
```

Covered witnesses:

- setup → status, including a second `Workspace::open` over the same app data.
- empty `order` increments the revision exactly once; another composition sees it.
- stale `order` returns `StalePlan` with no second increment.
- `import preview`/`apply` return `StageUnavailable`, revision/members unchanged.
- deploy preview/apply and restore return the typed `UnsupportedTarget` blocker.
- malformed/unknown-field DTOs return `InvalidRequest` without catalog revision or payload writes.
- `Request` refuses `shell`/`write_file` operations; CLI has no such subcommand.
- CLI `--json` emits one envelope; a seeded revision `9007199254740993` and member
  `9223372036854775806` round-trip as exact strings and the revision increments to
  `"9007199254740994"`; DTO serialization keeps IDs and revisions above `2^53` as strings.

The final commands were repeated from a clean `git archive HEAD` copy of this commit because the
shared checkout gained unrelated uncommitted `crates/platform/src/linux.rs` work during this task;
that work is not part of this commit.

Not run: Windows workflow execution (hosted CI matrix), native import stage, native
deploy/restore, recovery reconciliation and any real game write.

## Remaining work (phase B / Task 8)

- `app/` is untouched: Tauri 2 commands, capability/CSP, official Dialog pickers, typed
  transport, TS mirrors checked against Rust envelopes, workspace UI, ordered progress
  channels, restart/reconnect status refresh and WebDriver checks.
- Connect a qualified native library stage before `import apply` can publish; keep the current
  `StageUnavailable` gate until then.
- Connect and qualify the native effect/durability port before deploy preview/apply or restore
  can do anything; keep `UnsupportedTarget`/`Configuration` until then.
- Add explicit startup reconciliation for recorded pending work under the same recovery gate.
  Current phase-A status reports pending work and deliberately does not resume it.
- Reuse the same `Workspace`, `Request` and envelope DTOs from the thin Tauri command layer;
  desktop selection IDs must resolve to owned sources in Rust, never to a renderer-supplied path.
