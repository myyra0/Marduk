# Marduk

Cross-platform mod manager. Rust-first, Tauri 2 desktop shell, MO2-inspired UI.
A fresh implementation that replaces a legacy app. The legacy source is a **behavioural reference** at
`$LEGACY_PATH` (env var). Call it "legacy" everywhere; the old product name must not appear in code, UI, strings, config or docs.

## License and reuse

Marduk is GPL-3.0-only (`LICENSE`). Reusing GPL-3-compatible code is allowed, including adapting legacy, MO2 or Vortex
logic, on these terms: adapt to this architecture rather than transplanting files; preserve the capability's behaviour and
safety rules; do not add per-file provenance banners or "copied from" comments (the `LICENSE` file carries the license for
the combined work); record what was adapted and why in the owning packet's report. Do not copy code that implements a
weaker guarantee than this project's contracts — that includes delete-then-rename replacement, which destroys displaced
bytes. Trivial logic is still written fresh.

## Documentation and claims

One source per rule. A guarantee lives in one contract, is enforced by a test or a named syscall/flag, and code comments describe
only what that function does plus a pointer to the contract. Never restate the same rule in a second file: two copies drift and the
wrong one is what people read.

State a guarantee only if it is enforced. If the code detects a condition after the fact, write "detects" and name the residual.
"Cannot", "never", "always", "only" and "prevents" require a test or a syscall mode behind them; otherwise they are a defect to fix,
not wording to soften.

Evidence belongs in committed docs (`docs/research/`), never in the gitignored ledger. A packet ledger records progress and rulings;
it is not proof and must not be cited as one. Decisions that change a guarantee, licence or supported configuration get a dated ADR
with the cost if the decision is wrong.

Availability is evidence-backed: a capability is `available` only where a recorded runtime result exists for that exact configuration.
A static check (filesystem type, target OS, compile success) is never sufficient on its own, and an evidence row may not move from
`not run` to `passed` without the run behind it.

## Scope (current phase)

- Goal: functional parity with legacy. Nothing else is required.
- No compatibility with legacy installs, configs, data, internal APIs or plugin code.
- Preserve useful behavior, not internal mechanisms. Prefer simpler designs when they meet the same requirements. Explain consequential differences; ask before dropping a retained behavior, narrowing supported configurations, or weakening a safety guarantee.
- Put small, safe improvements in the owning execution packet with an acceptance check. Optional tangents go to
  `docs/backlog-improvements.md`; blocked existing capabilities go to `docs/backlog-deferred.md` and still count toward parity.
- Not built now, but the design must leave room for them: plugin system (designed fresh, not ported), Reloaded-II support
  (reuse its runtime loader first; a native loader is speculative), an MCP interface that lets an AI manage mods
  (needs permissions/access control), more operating systems.
Design check: each should fit as a new adapter, module or client without broad refactoring. A justified interface extension is allowed when a real requirement appears; do not build speculative interfaces to avoid it.

## Architecture

Dependencies point downward only.

1. **core**: domain model and engine. Profiles, mod library, priority and conflict resolution, dependencies,
   deploy/restore, transactions. No UI, OS or network types in its public API.
2. **adapters** (traits owned by core): platform (game discovery, launching, paths, link/copy primitives; one impl per OS),
   games (one handler per game), mod sources (Nexus, Thunderstore, collections/Wabbajack, ...), archives,
   deploy methods (interchangeable).
3. **frontends**: the Tauri app (thin command layer) and a CLI, both calling core through the same typed API.

Rules:
- Game logic never touches OS specifics. OS code never knows about a specific game.
- Adding a game, source, deploy method or loader means adding a module and registering it, never editing a central switch.
- Define interfaces from present requirements, not hypothetical ones. No large multi-purpose modules.
- UI and backend talk only through the typed API (commands and events). No UI types in core, no backend internals in the UI.
- Every module must be testable alone with fakes (synthetic game dirs, archives, profiles). Each extension point that exists has a fake; add one with the seam, not ahead of it.

Provisional layout (finalised by the Phase 0 audit): `crates/{core, platform, games, archive, storage, workspace, cli}`,
`app/` (Tauri 2 + frontend, separate workspace). Frontend stack is chosen in Phase 0.

## Language allocation

Rust by default: filesystem, deploy/restore, conflicts, dependencies, archives, anything performance-sensitive.
Python only where Rust gives no real advantage and Python is clearly simpler; keep that boundary narrow and explicit.
Reuse legacy code for complex or error-prone logic, adapted to this architecture. Trivial logic is written fresh.
Reuse sound existing Rust components from legacy instead of rewriting them.
Rust is the default, including for hard problems; difficulty is never a reason to reach for Python. Python only where it is genuinely the better tool (dev tooling, glue, a Python-only dependency). Python in the shipped app needs an ADR: why Rust is a poor fit, and how it is bundled per OS.
Tauri 2 only: capabilities/permissions model, no v1 APIs (`allowlist`, `tauri::api`).

## Library and platform research

Use Context7 (`query-docs`) before writing a third-party library or framework API, especially when the dependency
version may be newer than training data: Rust crates (for example `zip`, `rusqlite`, `windows-sys`), Tauri 2, and the
React/Vite/Vitest stack. It informs syntax and current API shape, never behavior: the brief, contracts and repo docs
remain authoritative, and a query result does not authorize a narrower guarantee or a different design.
For OS primitives and filesystem semantics (`renameat2`, Btrfs ioctls, Win32 durability), prefer the locked crate
source, local kernel headers/man pages and current vendor documentation; Context7 is not authoritative there.
If Context7 is unavailable, read the locked crate source in `~/.cargo/registry/src/` and record that path in the report.

When an implementation is stuck, or a native/API problem looks like it has a known solution, tell the subagent to research it before inventing one: GitHub code/issues via the GitHub MCP, the web via Firecrawl/search, and current vendor documentation. Prefer maintained crates and documented OS patterns; record the source and licence in the report. Research informs the design, not the acceptance criteria: the brief, contracts and frozen tests still decide what is correct, and copied code must fit this project's architecture.

## Reliability

Anything that modifies a game install or user files is transactional: journaled, validated, reports progress, and can be
rolled back or resumed after a crash or interruption. Restore must always work.

## Platforms

Windows and Linux are both first-class from day one. OS-specific behaviour lives only in the platform adapter. Platform code is unverified until the Windows and Linux CI matrix is green.

### Windows test VM

A local, disposable Windows VM provides native NTFS evidence. It is isolated from the host and holds no real data; do not reuse its password anywhere else.

- Libvirt: `virsh -c qemu:///system list --all`; domain `win10-ltsc`; `start` and `destroy` control it.
- SSH: `admin@192.168.122.50`, password `0000`. For non-interactive use, put the password in an askpass script and run
  `SSH_ASKPASS=<script> SSH_ASKPASS_REQUIRE=force setsid -w ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o PreferredAuthentications=password -o PubkeyAuthentication=no admin@192.168.122.50 <command>`.
- Checkout: `C:\marduk`. Sync one commit with `git archive HEAD | gzip`, copy it over, then `rmdir /s /q C:\marduk\crates` and `tar -xmzf` in `C:\marduk`. Git archive preserves mtimes; without the rmdir, Cargo silently reuses stale rlibs.
- Run tests through a scheduled task as `admin` (`schtasks /ru admin /rp 0000`), redirect to a log on the VM, then read the log. `cargo clean` once if crate metadata is corrupted (`E0786`/`LNK1207`).
- Treat any Windows result as evidence only when the log names the test and its `test result:` line; an advisory CI job is not a substitute.

## Design defaults

- **Prefer a product constraint to a coordination protocol.** One client runs at a time, and one catalog owns an installation. Do not add simultaneous-client or multi-writer support without a milestone requirement; a lease, lock hierarchy or compare-and-swap needs a reason to exist.
- **State that describes an installation lives with it.** An ownership marker belongs in the installation, not in a per-user directory derived from `HOME`/`XDG_*`; then every client sees it without key derivation or environment agreement.
- **Interrupted work rolls back and is redone.** Do not resume by adopting an artifact whose ownership is unproven. Retained originals are the rollback source; a new attempt starts clean.
- **When a review finding is really a scope question, delete the feature before hardening it.** Reducing surface area is a valid fix.

## Mod ordering

As in MO2: lower-priority mods at the top, higher-priority mods (which overwrite them) at the bottom.

## UI / UX

MO2-familiar workflows, but a reorganised, consistent, clean layout. This is not a Qt port. Functionality and clarity over
decoration. Redesigned onboarding. Room for future features without clutter.
Built-in themes: Light, Dark, Catppuccin, Dracula, Nord, from one consistent theme system. Legacy's default theme is not carried over.

## Branding

Name: Marduk. Use a placeholder icon until the real one is supplied.
Commit messages, branch names, code, comments, UI and user-facing docs never mention porting, the legacy app, or its name. Describe the capability added (a commit-msg hook rejects violations).

## Agent roles

Use `subagent-driven-development` for approved, ready execution packets. One top-level packet goes to one orchestrator in one session; the orchestrator owns all subtasks, verification and handoff. On interruption, resume that packet from its ledger and actual workspace evidence, not conversation history.

| Role | Model — all at `high` | Assignment |
|---|---|---|
| `architect`, `escalation` | GPT-6.2 Sol (`MODEL_SOL`; `openai/gpt-6.2-sol#high`) | Architecture, shared contracts, risk-intensive acceptance tests, recovery and trust decisions, final whole-branch review; escalation implementer after two failed cycles on the same task |
| `orchestrator`, `worker-standard` | DeepSeek V4.1 Flash 1017 (`MODEL_DEEPSEEK`; `opencode-go/deepseek-v4.1-flash#high`) | Session controller, integration, moderate multi-module implementation, routine independent review, simple planner-owned tests |
| `worker-simple` | Beam Free (`MODEL_BEAM`; `opencode-go/beam-free#high`) | Very small, bounded implementation and mechanical checks when the approved brief and tests decide the behavior |

Every subagent dispatch must use the `#high` variant of the exact model ID shown above. A base ID without `#high` is a preflight failure: do not dispatch it, and do not substitute a different model or effort silently. The orchestrator records the exact dispatched model string in the ledger.

Tier order is Beam for very small bounded work, DeepSeek for orchestration and moderate work, Sol for architecture and risk and as the escalation implementer.

Use only these models. Select the exact configured ID and high effort for each dispatch; do not infer IDs from labels or silently substitute a different model or effort. A missing binding is preflight failure, not permission to run the supervisor or bypass review.

- Give Beam only very small briefs with exact files, interfaces, expected failures and check commands. Route ambiguous integration, moderate work or a multi-module change straight to DeepSeek; route consequential transaction, recovery, licensing and security decisions to Sol rather than buying retries.
- Independent review starts at DeepSeek. Require Sol review for risk-intensive contracts, platform recovery, security and the final whole-branch review. Do not let a writer approve their own change. Combine blocking findings from informed and blind reviews when a blind review is warranted by risk.
- Review from the approved brief, exact diff and frozen tests first. Give reviewers command outcomes and evidence paths, not writer transcripts, rationale, prior verdicts or copied diffs. Keep earlier findings and rulings for reconciliation after the independent pass; use a separate blind review when risk warrants it. Return verdicts, actionable findings (`path:line`, impact, fix) and verification gaps only. Put supporting evidence in the ledger, not in repeated chat summaries.
- Freeze planner-supplied acceptance tests. Sol authors high-risk tests; DeepSeek can author routine ones. Workers may add tests but must not edit the provided oracle without a separate approved correction. An unauthorized change to it is a Critical finding.
- Count a failed cycle when an implementation attempt plus its verification/review leaves Critical or Important findings, or fails an acceptance check for reasons attributable to the change. After **two failed cycles on the same task at one tier**, promote to a fresh worker one tier up: Beam → DeepSeek → Sol; Sol implements the escalated task, it does not only review it. Hand over the diff, test output, review findings and ledger; do not restart discovery. The same worker may take the first fix. At five total failed cycles, park the packet, record evidence and stop dependent work. This project rule takes precedence over the skill's default promotion timing.
- Provider limits, missing environments and gate failures are not failed code cycles. Resume at the same tier if practical, or use the next allowed tier if justified; record the cause. A wrong or unsettled contract, unsafe action, or new product choice requires a ruling or user decision, not more retries.
- Do not run parallel writers in the same paths. Only a packet marked `ready` can be dispatched. No product work, runner changes, Git setup, or distribution from a planning-only session.

## Docs (sources of truth; update, don't duplicate)

`GLOSSARY.md` (terms), `docs/PLAN.md` (roadmap), `docs/inventory.md` (parity and evidence), `docs/architecture.md` (design and gates),
`docs/adr/` (ADRs), `docs/contracts/` (contracts), `docs/research/` (research and evidence),
`docs/superpowers/plans/` (execution packets), `docs/backlog-improvements.md`, `docs/backlog-deferred.md`.
