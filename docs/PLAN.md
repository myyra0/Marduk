# Marduk: Rewrite Plan

The [full-parity work plan](superpowers/plans/2026-11-20-full-parity.md) is the execution index. It links the inventory, design gates and session packets; this file owns the product roadmap. Planning documents do not authorize implementation.

## 1. Objective

Rewrite the legacy mod manager as **Marduk**: a modern, Rust-first, Tauri 2 application with better performance, reliability,
maintainability and user experience, and a foundation for multiple operating systems, games, mod sources and mod-loading ecosystems.

This is a genuine architectural overhaul, not a translation of the old implementation. Preserve its useful functionality,
behaviour and accumulated game compatibility while replacing weak architecture and improving the experience.

Marduk is a fresh implementation. Compatibility with legacy installations, configuration formats and user data is not required.
Accept it where it comes naturally, but do not add significant complexity solely to preserve it.

**Guiding priority:** a correct, reliable, maintainable application with complete existing functionality, not a rushed port or a
collection of unrelated new features.

## 2. Scope and priorities

**Full functionality parity with the legacy application is the goal.** Implementation may be prioritised and staged, but lower
priority does not mean optional, and deferral does not mean removal from the parity goal.

The legacy feature inventory (`docs/inventory.md`) must account for every existing capability, including less visible features.
Track each as implemented, in progress, deferred or intentionally dropped, with its priority and rationale. Only drop an existing
capability through an explicit, justified product decision, not because it is inconvenient to implement. **Full parity means all
retained legacy capabilities are implemented and verified across P0, P1 and P2**, not merely that the highest tiers are complete.
Do not claim parity while retained features remain deferred.

Priorities guide implementation order; the legacy audit determines final assignments:

- **P0 — essential workflows:** game discovery, mod installation and removal, profiles, mod priority and conflicts,
  safe deployment and restoration, onboarding, settings.
- **P1 — major supporting functionality:** downloads and archive formats, Nexus integration, additional game handlers,
  Thunderstore and other important integrations identified in the audit.
- **P2 — remaining existing functionality:** private-view deployment by supported platform/game, collections/Wabbajack, game-specific wizards, headless interfaces and remaining
  handlers or integrations not assigned a higher priority.

Build in coherent, working increments, validating high-risk assumptions early. A staged release may have an explicitly documented
scope without being described as full parity.

## 3. Core technical decisions

### 3.1 Functionality first; redesign where it helps

Focus the rewrite on existing functionality rather than major unrelated features. Preserve useful behaviour where practical,
without copying the legacy architecture. Correct structural problems and make sensible, low-risk improvements as components are
rebuilt.

### 3.2 Rust first; Python where genuinely appropriate

Rust is the primary language, especially for the core engine, filesystem and file management, deployment and restoration,
conflicts, dependencies, archives, performance-sensitive work, and operations where correctness, safety or concurrency matter.

Do not switch to Python merely because Rust implementation is difficult. Equally, do not force Rust onto a task for which its
libraries or ecosystem are genuinely unsuitable. Python is appropriate for development tooling, one-off checks, isolated
low-risk scripting or glue, and cases where a necessary capability has no acceptable Rust solution. Prefer Rust when both are
reasonable, particularly for shipped functionality.

- Keep performance-, safety- and reliability-critical operations in Rust.
- Python shipped with the app requires a documented justification and a practical interpreter/bundling plan for each supported OS.
- Python code can be written from scratch where appropriate; it need not be copied from the legacy app.
- Selectively reuse or adapt small pieces of legacy code where worthwhile, rather than copying modules wholesale. Reuse can help
  with complex, error-prone behaviour; simple code may be better rewritten. Check licences and record reused code.
- Assess existing Rust components for reuse or improvement rather than rewriting them automatically.
- Keep language boundaries clear, limited and maintainable.

The goal is a sound implementation for each component: **Rust-first, not Rust-at-all-costs or Python-by-convenience**.

### 3.3 Application and UI architecture

Use **Tauri 2**, including its capabilities and permissions model; do not use Tauri 1 APIs. Keep the frontend and backend clearly
separated through typed commands, events and structured results, without coupling UI components to backend internals. The core
must also be accessible to other clients, such as a CLI or future integrations.

Choose a maintainable frontend stack with strong Tauri 2 support, consistent theming, good UX and efficient virtualised views for
large modlists.

## 4. Architecture and extensibility

Use small, cohesive, independently testable components with clear boundaries. A useful separation is:

1. **Core:** domain model and engine — mods, profiles, dependencies, priority, conflicts, deployment, restoration and
   transactions. Public domain interfaces do not depend on UI frameworks, OS-specific types or network clients.
2. **Adapters:** platform/game discovery and launching, OS-specific filesystem behaviour, game handlers, mod sources, archives,
   loaders and deployment methods, connected to the core through well-defined interfaces.
3. **Clients:** the Tauri application and other interfaces to the same engine, including CLI and future external integrations.

Keep game-specific logic independent of OS-specific code. Mod sources and deployment methods should be replaceable or extensible
without changing unrelated subsystems. Adding a game, source, loader or deploy method should normally require a self-contained
module and registration, not edits throughout a central monolith.

Prefer interfaces justified by real requirements. Avoid both tight coupling and speculative abstraction. Future capabilities
should fit without broad core refactoring; **absolute public API immutability is not a requirement** when a well-reasoned extension
is needed.

## 5. Platform and future capabilities

Account for these requirements in the architecture without prematurely implementing speculative systems.

### 5.1 Cross-platform support

**Linux and Windows are first-class targets**, not a Linux design with Windows added later. Support both fully, isolate
OS-specific behaviour, and leave room for other operating systems in the future.

### 5.2 Reloaded-II support and potential replacement

A major future goal is native management of Reloaded-II mods: installation, configuration, dependencies, profiles,
enabling/disabling and load order, without needing its separate management UI. This is not part of the initial functionality port.

Distinguish its management application from its runtime loader:

- First consider using the existing runtime loader where it provides the necessary control and compatibility.
- A Marduk-compatible replacement loader and bootstrapper is a possible later direction, not a committed feature; its value
  depends on feasibility and adoption.

The initial architecture must leave sensible seams for mod ecosystems, loader and launch integration, per-game configuration,
dependencies and load-order behaviour. Assess these interfaces during architectural design without building Reloaded-II-specific
infrastructure prematurely.

**Before implementing Reloaded-II support**, investigate feasibility, technical complexity and compatibility requirements for
both using the existing runtime and building a replacement, then choose an approach based on evidence. That investigation and
implementation belong to a future milestone.

### 5.3 MCP support

Plan for comprehensive future **Model Context Protocol (MCP)** support, with the long-term possibility of an AI managing modding
through Marduk. It is not part of the initial functionality port.

The core should expose explicit, validated operations; structured results and errors; useful progress and events; and permission
boundaries that do not presume the caller is the UI. A future MCP server should be another client of that engine, rather than a
separate implementation of mod-management logic. Define the required capabilities and trust boundaries during architecture
planning, without building the server now.

### 5.4 Plugin system

A manager plugin system is a future capability. The parity target is the pinned base app, including bundled behavior regardless
of how it is implemented internally. Third-party manager plugins, their store/host and their APIs are outside this phase. This
does not exclude game-content plugins such as Bethesda load-order files.

### 5.5 Other extensions

The design should accommodate new games, mod sources and download providers, mod-loading frameworks, deployment methods,
third-party plugins, external integrations and platform-specific functionality without sprawling changes to unrelated systems.

## 6. Architectural decisions to resolve

Record consequential design choices and their trade-offs in `docs/adr/`. The initial architecture must resolve:

1. **Deployment per OS:** supported link, hardlink and copy approaches, permissions and filesystem limitations, and whether
   virtual-file views are applicable per game/launcher/OS. Preserve user selection among supported deployment modes. Copy
   is the first vertical slice, not a product-wide default. See [deployment research](research/deployment.md) for the Linux
   shadow/OverlayFS/FUSE comparison and the Windows alternatives.
2. **Transactions and recovery:** safe write boundaries, journaling, rollback/recovery guarantees, and behaviour after crashes or
   power loss. This is a critical risk area.
3. **Cross-platform paths:** case sensitivity, long paths, reserved names, normalization and collision handling.
4. **Persistence:** profiles, mod library, settings and the layout of mods, downloads and overwrite data.
5. **Frontend and core API:** frontend stack, typed operations, events and separation of concerns.
6. **Errors and diagnostics:** consistent error handling and logs sufficient to investigate failed operations.
7. **Language and reuse:** any shipped Python dependencies, distribution implications, and licence status of reused code.

The goal is to make and document sound decisions, not to mandate a particular number of ADRs or an unnecessarily rigid process.

## 7. Security and trust boundaries

- **Archives:** prevent path traversal and absolute-path extraction; handle embedded links, case collisions and decompression
  bombs safely.
- **Downloads:** use secure transport and verify hashes when provided by the source.
- **Credentials:** use the OS keyring for API keys; never store secrets in plain configuration or logs.
- **Tauri:** least-privilege capabilities, narrow filesystem/shell access, minimal exposed commands and a restrictive CSP.
- **Game directories:** modifications pass through the safe deployment/transaction layer.
- **External APIs:** respect provider terms and rate limits and cache appropriately.

## 8. Legacy functionality and compatibility reference

Thoroughly inspect the old codebase and inventory all its behaviour, including game handlers and discovery; mod installation,
removal, enabling and disabling; profiles; deployment and restoration; conflicts and priorities; downloads and archives; Nexus;
Wabbajack and collections; Thunderstore and other sources; game-specific wizards and integrations; existing APIs and headless
features; and anything else present outside the primary UI.

Use the pinned, read-only legacy tree in [the inventory](inventory.md) as the **base-app behavior reference**, not an
architectural template. Use MO2 for Windows-specific research only; do not expand the parity target to match MO2. Internal APIs,
structures, configuration formats and installation compatibility need not be preserved. GPL-compatible reuse is allowed for
evaluation; a specific artifact's provenance and distribution compatibility must be confirmed before integration.

## 9. Scope control and controlled improvements

Full existing-functionality parity remains the objective, but an unnecessarily rigid feature freeze would also waste opportunities
to make the rewrite meaningfully better.

- **Small improvements:** build straightforward, valuable and safe usability, feedback, validation, accessibility and reliability
  improvements alongside the feature they improve, even when no equivalent exists in the reference. Put them directly in the
  owning packet with an acceptance check if they introduce no new external dependency, sensitive effect, or milestone blocker.
  Touching more than one module is not automatically disqualifying; assess actual complexity, risk and effort instead.
- **Completing partial legacy features:** finish unfinished behaviour when its intent is clear and completion is reasonably
  straightforward, safe, testable and unlikely to derail broader functionality work.
- **Improvement backlog** (`docs/backlog-improvements.md`): record optional ideas that would widen scope or divert the rewrite;
  do not duplicate improvements already owned by a packet.
- **Deferred-feature backlog** (`docs/backlog-deferred.md`): track retained legacy functionality that cannot sensibly be completed
  in the current stage. Record what exists, what is missing, why it is deferred, dependencies and approximate complexity.
  Deferrals remain part of the parity goal until implemented or explicitly reconsidered.

Prioritise reliable, complete functionality. Implement modest improvements when appropriate; document larger ideas for later
rather than letting them take over the rewrite.

## 10. Testing and verification

Follow genuine **test-driven development** for substantial functionality: establish expected behaviour before implementation,
write tests that could expose an incorrect result, and verify functionality rather than merely testing for a passing checkmark.
Compilation alone does not establish correctness. Use mutation checks or equivalent challenges where valuable for critical logic.

Testing must cover unit, integration, regression and UI workflows; installation and removal; profiles, dependencies and
conflicts; deployment and restoration; archive and filesystem edge cases; and performance. Include interruptions, failed
operations, application crashes, recovery and restoration.

- **Legacy comparison:** use equivalent fixtures and cases against old and new behaviour where practical; preserve representative
  expected outputs when direct comparison is not possible.
- **Reproducible test environments:** synthetic game directories, mod archives, profiles, conflicts and deployment scenarios
  should support meaningful tests without requiring real installed games. Exercise important extension interfaces with
  representative test implementations where that validates the design.
- **Platform verification:** maintain automated Linux and Windows testing for platform-specific behaviour and cross-platform
  compatibility.

## 11. Performance and reliability

Measure the legacy application's startup, game/mod discovery, file scans, conflict detection, large modlists, archive processing,
installation, deployment, restoration and memory use. Use repeatable benchmarks to identify real bottlenecks and compare the
rewrite, rather than chasing hypothetical optimisations.

Operations touching game installations or user files must prioritise correctness: validation, visible progress, clear errors,
transaction tracking and recovery. Destructive changes must be recoverable after interruption or failure. Restoration must be
reliable, not a best-effort afterthought.

## 12. UI and UX

Give Marduk a substantial UX overhaul while remaining strongly **MO2-inspired**. Preserve familiar, useful mod-management
workflows but do not reproduce the legacy Qt UI or its disorganised structure. Make setup and onboarding straightforward.

Prioritise functionality, clarity and usability over decoration. The interface should be coherent, predictable, responsive,
performant, approachable to MO2 users and able to accommodate future features without clutter. Organise legacy screens and
workflows into a consistent structure; virtualise large lists.

- **Mod ordering:** lower-priority mods at the top, higher-priority mods (which overwrite them) at the bottom, as in MO2.
- **Themes:** built-in Light, Dark, Catppuccin (all flavours), Dracula and Nord in one consistent theme system. Do not retain or
  install the legacy default theme.

## 13. Documentation

Keep architectural decisions, functionality coverage, progress and deferred work discoverable without competing sources of
truth. Core project records are those listed in `AGENTS.md` under "Docs (sources of truth; update, don't duplicate)"; this plan
is one of them. Implementation packets and checkpoint evidence link these records rather than duplicating decisions, feature
status or backlog entries.

## 14. Planning outcomes and implementation stages

Start with a thorough legacy audit and architectural assessment. Planning should establish:

- A complete, prioritised functionality inventory and an assessment of legacy architectural weaknesses.
- The proposed Rust/Tauri/frontend design, language boundaries and Linux/Windows platform strategy.
- Deployment, persistence, error handling, security and recovery decisions.
- A testing and performance baseline strategy.
- A concise assessment of the architectural seams for later Reloaded-II and MCP integration.
- Initial improvement and deferred-feature backlogs, plus staged implementation scope and acceptance criteria.

Plan the full retained scope together; execute it incrementally, proving each dependency before expanding coverage. A planned
packet is not ready if its contract or prior proof is open. One orchestrator session owns each top-level packet and verifies
its worker subtasks; see `AGENTS.md` for model routing and review escalation.

The first working slice is one representative cross-platform game (Skyrim SE loose files proposed), local ZIP import, one
profile, priority and conflicts, journaled copy deploy/restore, a minimal desktop flow and a matching CLI. Process interruption
and abrupt power loss are separate recovery checks for named local configurations. Broader platform, provider, tool and
private-view support follows without changing the parity target:

| Stage | Scope | Acceptance focus |
|---|---|---|
| **M0 — Audit and design** | Legacy inventory, architecture, key design decisions and verification approach | All existing capabilities accounted for; major risks and extension boundaries understood |
| **M1 — Working core** | Core engine, representative game support, platform abstraction, minimal UI, safe deploy/restore | End-to-end installation, deployment and restoration with meaningful recovery tests |
| **M2 — Essential workflows** | Complete P0 on Linux and Windows, including onboarding and settings | P0 behaviour verified on both platforms |
| **M3 — Major integrations** | P1 downloads, archives, sources and additional game handlers | P1 behaviour verified, including failures and recovery |
| **M4 — Remaining parity and hardening** | Private views per platform, retained P2 functionality, broader compatibility, UX, recovery, security and performance | All retained capabilities verified before claiming full parity; regressions and recovery covered |

If an intermediate release excludes a retained feature, record that scope explicitly and keep the missing capability tracked.
Completion of an intermediate stage is not permission to silently discard deferred functionality.

M1 is accepted at `9418548` by a Sol whole-packet pass. The acceptance covers the M1 workflow on
Linux tmpfs/Btrfs and Windows NTFS. It does not claim power-cut durability, a real Skyrim SE write,
ext4, Btrfs cross-subvolume, an installed-shell write path or the Windows launched shell; those gaps
are recorded in `docs/m1/limitations.md` and stay tracked for the stages that own them.

## 15. Naming and branding

The application is **Marduk**. Use a placeholder icon until the final icon is supplied; the visual identity is handled separately.
The product, codebase and user-facing material should use the new identity without retaining the old application's branding.
Internal planning and compatibility records may refer to the *legacy application* where needed, but do not propagate its old name.

---

**Overall goal:** a fundamentally better successor: a Rust-first, Tauri-based, cross-platform mod manager that retains existing
capabilities while improving architecture, performance, reliability and UX. Build a strong foundation for future mod-loading
ecosystems without letting speculative features or implementation shortcuts compromise the core application.
