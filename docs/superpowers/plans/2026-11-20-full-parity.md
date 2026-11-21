# Marduk Full-Parity Implementation Plan

> **For agentic workers:** Use `subagent-driven-development` for approved packets. Follow the project-specific model and review rules in `AGENTS.md`. This is the full-plan index, not a dispatchable packet. Do not start an open or unapproved packet.

**Goal:** Deliver the retained base-app outcomes from the pinned reference on the applicable Linux and Windows configurations, with recoverable writes and no third-party manager plugin host.

**Architecture:** A Rust core plans and validates operations. Game, platform, storage, source and archive adapters handle effects; CLI, headless and Tauri clients consume the same operations. Physical and private-view deployment share resolution but not platform mechanics.

**Tech Stack:** Rust, Tauri 2, proposed React/TypeScript/Vite, local SQLite. Verify exact versions and the build matrix before freezing their packets.

**Spec:** [Preparation design](../specs/2026-11-20-m1-handoff-design.md), [architecture](../../architecture.md), [inventory](../../inventory.md).

## Global Constraints

- The pinned local reference is read-only. Windows-only MO2 research is not a second parity baseline.
- Common workflows require evidence on Windows and Linux. Platform-native routes are recorded per applicable game/configuration. Never substitute unrelated OS evidence.
- Store game and user changes under a recoverable operation protocol; no worker may widen mutation scope silently.
- No migration, old wire compatibility, third-party manager plugins, Reloaded-II integration, or product MCP server.
- Preparation and prototype approval do not authorize releases, real provider/account actions, broad external writes, Git setup or configuration changes.
- The orchestrator's unit is one top-level packet per session. Parallel worker writes must not overlap. Approved shared tests, specifications and contracts cannot be weakened by an implementer.

## Review Focus

- Prior backup or root unavailable during restart: leave state actionable, never rediscover a different writable root and continue. M1-R02, M2-D01.
- Package filename appears equivalent on only one filesystem: refuse ambiguous candidates without changing user files. M1-R01, M2-I01.
- Provider replaces the object during restart/resume: stale partial or approval cannot be reused. M3-S01, M4-S01, M4-S03.
- Game/tool writes through its deployed path: never corrupt the original or immutable payload through an alias. M2-D02, M4-X01, M4-X02.
- Last parity gap hidden by compatible UI or broad green tests: require a positive witness per retained outcome and supported route. M0-A04, M4-A04.

## Checkpoints

| Checkpoint | Accept when | Failure disposition |
|---|---|---|
| C0 — Audit and design freeze | Each audited surface is reconciled against its denominator, including 299 headless registrations, desktop and packaging actions, real handler registrations and reachable tools. Each retained outcome has an owner and witness. Consequential decisions are accepted or explicitly gated. | Leave affected packets `blocked`; don't call a static candidate list complete. |
| C1 — Working core | Synthetic Skyrim SE loose-file workflow works through desktop/CLI; priority winner, exact file restoration, external-edit conflict, and isolation verified. Named Windows and Linux recovery evidence passes G-RECOVERY. | Keep failing gates open; don't enable real-install writes based on partial results. |
| C2 — Essential workflows | Install/configure/onboard/deploy/restore and run supported first games on both platforms, including supported physical modes and profile operations; launch/process conflicts and packaging smoke tests pass. | State release scope exactly; no “full parity.” |
| C3 — Sources and major supporting workflows | Downloads, formats, first providers, game-content plugin and profile workflows, extra root writes and data views all have deterministic, offline fixtures and platform checks. | Live credentials, external packages and unverified environments remain separate gates. |
| C4 — Full retained parity | Matrix ties every retained outcome to implementation and verification on applicable configurations; Linux and Windows private-view routes and compatibility gaps explicitly resolved. Final independent Sol review and cross-platform release evidence. | No parity claim while an outcome remains unverified, deferred or unresolved. |

## Packet catalog and readiness

- [M1: Working core](2026-11-20-m1-working-core.md) describes the first vertical slice and reference-test decisions.
- [M2–M4: Full-parity packets](2026-11-20-m2-m4-parity-packets.md) assigns later work now. Gate dependencies are explicit; the catalog is not independent acceptance of each affected contract.
- [Architecture gate register](../../architecture.md#decision-and-proof-register) owns decisions and proof requirements. [Backlog](../../backlog-deferred.md) names retained gaps; a backlog entry is not permission to park a failed acceptance check.

States for packets: `draft` (interface or brief not frozen), `blocked` (name the gate and owner), `ready` (approved brief and starting state checked), `running`, `verified`. Only `ready` is dispatchable. Predecessor listed with the same checkpoint is a packet dependency; no overview state implies its completion. Candidate targets, test commands and implementation paths in this plan are prospective until scaffold and contracts are reviewed.

### Remaining audit packets (M0, this planning pass)

| Session | Deliverable and subtasks | Exit evidence |
|---|---|---|
| M0-A01 — External surface reconciliation | Pin process; parse the headless registry's exact keys, flags and schemas. Trace desktop action registrations and CLI separately. Map each to user outcome, ownership and witness. Resolve overlap without collapsing distinct behavior; review manager-plugin calls for bundled functionality. | Counts reconcile to 299 at the pinned revision; no unassigned registrations or unclassified desktop/CLI actions. Source-backed behavior separated from documented-only, untested and out-of-scope. |
| M0-A02 — Games and tool matrix | Reconcile manifest against discoverable handler classes and definitions, including editions, inherited rules, store paths, supported modes, launchers and sandbox/runtime variants. Trace wizard registrations and external prerequisites against each reachable game. | Exact registry and package/route matrix; one test fixture recipe per distinct behavior and named unsupported/unknown routes. 72 literal candidates and 31 directories are starting evidence, not target counts. |
| M0-A03 — Difficult work streams | Trace archives/installers and unshipped branches; replayable provider contracts; collection and Wabbajack effects; source and adapter licensing; deployment path and generated-file cases. Separate package-wide support from real workflow proof. | Evidence links, policy choices and proofs on G-PATHS, G-RECOVERY, G-EXT and G-LICENSE gates. |
| M0-A04 — Audit and packet freeze | Reconcile inventory, exception backlogs, status and graph. Freeze exact check commands/files/interfaces per unblocked packet; author only the approved planner-owned test support/reference tests. Review one high-risk packet with a fresh reader. | Unowned rows = 0; all accepted scope changes visible; every packet either meets readiness rules or names the missing evidence/owner. |

Current audit evidence: [desktop/CLI trace](../../research/desktop-crosswalk.md), [headless crosswalk and job semantics](../../research/headless-crosswalk.md), [game/tool static reconciliation](../../research/tool-crosswalk.md), [game/tool fixtures](../../research/game-tool-witnesses.md), and [archive/installer trace](../../research/archive-crosswalk.md). Proposed M1 contracts cover [file resolution](../../contracts/m1-file-resolution.md), [ZIP/library publication](../../contracts/m1-zip-library.md), [native roots](../../contracts/m1-root-verification.md), [native proof](../../contracts/m1-native-proof.md), [copy deploy/recovery](../../contracts/m1-recovery.md) and [clients](../../contracts/m1-clients.md). Static coverage does not substitute for per-route or package checks.

The [headless ledger](../../research/headless-crosswalk.md#remaining-effect-audit) reconciles all 299 registry keys: **290 exact-key effect traces, nine excluded host registrations, none pending**. This closes static headless effects, not C0. The [first-run catalog snapshot](../../research/first-run-catalog.md) covers 52 data-only definitions and seven optional store entries. The [package route evidence](../../research/package-route-evidence.md) separates recipes, actual artifacts, installed outcomes and update origins. The [upstream provider review](../../research/provider-upstream-review.md) and [launch/tool review](../../research/launch-tool-upstream-review.md) inform the packet checks, not production or route proof.

- **M0-A01:** Appended Collection resume and revision changes are traced through desktop; do not infer them from standalone headless routes. The [desktop action ledger](../../research/desktop-action-ledger.md) reconciles 28 menu/action source files and adds prefix, provider-card, saved-view, notifications and cache-cleanup cases. The [preview and export draft](../../contracts/m4-previews.md) includes a reachable non-menu NPC export. Freeze exact UI/decoder fixtures with the effect owners; actual package reachability remains proof work.
- **M0-A02:** The [differential witnesses](../../research/game-tool-witnesses.md) cover multi-root and generated-file handlers, tool-facing exports, special title controls and archive-supplied routing. Share fixture mechanics only where effective behavior agrees; use independent per-title assertions. The [published packaging metadata](../../research/package-route-evidence.md#observed-publication-metadata) confirms x86-64 hosted Flatpak refs and a separate AUR recipe, not direct release artifacts, runtime behavior, arm availability, or the agreed release matrix.

The [Nexus detail](../../contracts/m3-nexus-and-versions.md#desktop-detail-and-exact-file-selection), [repository pickers](../../contracts/m3-repository-plans.md#desktop-selection-and-dependency-scope) and [support log](../../contracts/m4-support-diagnostics.md) have source-backed effect splits and proposed fixtures. Live rich-media and paste service policies, installed behavior, and packet approval remain separate gates.

The [cross-platform behavior checks](../../research/package-route-evidence.md#cross-platform-behavior-checks) specify what must match (identity, effects, consent and results) and what needs native proof. **C0** still needs decisions and reviewed fixture definitions; **first dispatch** needs an approved baseline, frozen first brief/oracle, independent review and execution preflight; **release proof** needs approved native recovery targets, installed artifacts and applicable provider eligibility. Progress in one is not sign-off for the others.

The [exact handler ledger](../../research/game-handler-ledger.md) anchors 72 static title candidates, and the [exact wizard ledger](../../research/wizard-action-ledger.md) anchors 65 keys, 80 catalog pairs and [per-handler triggers](../../research/wizard-action-ledger.md#handler-declarations-and-inheritance). That trace classifies static declarations, inheritance, named visibility gates and [delegated effects](../../research/wizard-action-ledger.md#delegated-desktop-effects); it does not establish a working runtime route. An observed ID collision or a missing runtime/package input stays a named gap, not an inferred removal or a pass.

The [conflict and requirement panel trace](../../research/desktop-action-ledger.md#conflict-and-requirement-panels) follows desktop callbacks into state, graph refresh and conditional auto-deploy. The [profile/group/save trace](../../research/desktop-action-ledger.md#profile-settings-groups-and-saves) adds exact owner, writable-copy and save-location distinctions. The [package entry-point trace](../../research/package-route-evidence.md#conditional-entry-points-from-the-pinned-source) adds startup, restart, update origin/branch, 32-bit repair and launcher access branches. None closes installed package proofs.

The [extender matrix](../../research/wizard-action-ledger.md#script-extender-acquisition-and-publication) follows 12 title/action bindings through source selection and publication. The [downgrade matrix](../../research/wizard-action-ledger.md#downgrade-artifact-and-output-boundaries) distinguishes the shared Fallout 3 patcher from the Steam game/Creation Kit helper pairs for Fallout 4 and Skyrim SE. Pinned selectors establish no independent compatibility, authorization to fetch binaries, or right to redistribute.

The [save-location trace](../../contracts/m4-saves.md#pinned-save-location-evidence) distinguishes the bundled manifest from the unavailable remote refresh, and adds differential store, shared-ID, native portable, override and profile-redirection cases. Full per-title route qualification and allowed manifest distribution remain open.

The [LOD and loader artifact trace](../../research/lod-and-loader-evidence.md) adds 13 pinned LOD action bindings, nine dedicated generator bindings, and the distinct SMAPI, SRML, MSCLoader, Special K, ME3 and BG3LE routes. It separates application install, discovery, launch, Restore, generated output and activation. Continue other desktop/title/package branches and tool-specific prerequisites; selectors do not establish current releases or compatibility.

The [Pandora and PGPatcher trace](../../research/lod-and-loader-evidence.md#pandora-and-pgpatcher) adds their four SE/VR bindings, distinct runtime prerequisites, output/config identity and tool-facing export. M4-Y03 records a single checked preparation step instead of two mutable PGPatcher config writes; that is a proposed implementation simplification, not evidence that either tool works in a shipped package.

The [profile snapshot and Quick Configure trace](../../research/desktop-action-ledger.md#profile-snapshots-and-quick-configure) records retention, explicit absence and option-specific effects. The [desktop/CLI package trace](../../research/package-route-evidence.md#packaged-desktop-and-cli-dispatch) distinguishes the separate entry points from AppImage's first-token dispatcher and shared stdio framing. Installed dispatch, package reachability and remaining desktop branches are still open.

The five retained browser schemes have [package ownership and transport evidence](../../research/package-route-evidence.md#browser-scheme-ownership-and-delivery) and [provider effect witnesses](../../research/desktop-action-ledger.md#browser-link-provider-effects). The [local archive trace](../../research/package-route-evidence.md#local-archive-entry-points) establishes picker and in-app drop, not OS file association. Browser/portal and native Windows ownership, named installed artifacts, actual OS file association and live provider eligibility remain separate proof gaps.

The [definition and instance trace](../../research/desktop-action-ledger.md#game-definitions-and-instance-configuration) separates draft/edit/delete, cached repository selection and activation, migration, scoped sandbox access, recorded-path Restore and physical cleanup. Failed migration or deleted-profile repair cannot stand in for verified recovery. Native migration/cleanup, the full catalog layout fixtures and any separate recorded-deployment abandonment policy remain open.

The [Skyrim SE texture-tool trace](../../research/lod-and-loader-evidence.md#skyrim-se-texture-tool-routes) separates three tool manifests, pinned runner prerequisites and output ownership. ParallaxR's patched DLL is identified, not cleared for redistribution; exact external archives/builds and installed Windows/Linux results remain open.

The [Bethesda application trace](../../research/bethesda-application-evidence.md) reconciles nine application families across 55 existing title/action attachments, plus explicit Wine registration and the Skyrim Together Play branch. CAO's writable staged mod, EasyNPC's generated merge, Wrye Bash's managed Restore, and Synthesis's separate setup/launch are not one common effect. It records Steam/GOG/VR path distinctions without implying native Windows parity or approving a runner or artifact.

The [title patch ledger](../../research/title-patch-evidence.md) traces nine controllers across 16 existing handler/action attachments, including New California's two Restores, Witcher merge inventory, Elden Ring baseline and output validation, and standalone archive repair/repack. The [Downloads callbacks](../../research/desktop-action-ledger.md#download-rows-location-settings-and-transfer-controls) identify an unbound ordinary pending install and distinguish saved Nexus Resume from live controls and explicit archive installation. [Welcome and folder selection](../../research/desktop-action-ledger.md#first-run-handoff-and-folder-selection) separate saved defaults and completion from setup, sign-in and package grants.

The [per-mod source trace](../../research/mod-source-action-evidence.md) separates local annotations, remote account effects, exact file identities, and writing bulk identification. [Picker branches](../../research/package-route-evidence.md#file-selection-export-and-late-replies) add origin and late-result witnesses through actual install/import/export callbacks. The [multi-root title evidence](../../research/multi-root-title-evidence.md) records Prey's store selector divergence, Cyberpunk's external settings mutation separate from REDmod, and RDR2 ASI/LML routing plus generated-order recovery.

The [profile exchange trace](../../research/profile-exchange-evidence.md) follows local export and share beyond the picker, tracks provider install versus bundled content/config/saves and incomplete import, and separates Collection packaging, transfer, draft registration and publication.

### Source audit handoff

Use this as the source handoff for the **inventoried** scope, not a claim that every dynamic desktop callback or every installed variant was exercised. The static denominators are frozen by the reference commit, not by these proposed acceptance checks. New evidence of an untraced retained effect reopens its owning row; package or native proof on an already traced route belongs to its gate.

- **Desktop and packaging:** the [settings trace](../../research/desktop-settings-evidence.md) separates immediate persistence, preview, Save/Reset, account workers, renderer setup and Restart/Restore. The [package apply trace](../../research/package-route-evidence.md#update-enrollment-origin-and-detached-apply) separates close authorization, detached dispatch, installed result and origin. M4-A03 owns exact package and association proof with M2-A01/D01. OS archive registration remains unestablished, separate from retained in-app installation.
- **Titles, tools and resources:** the [conditional title routes](../../research/game-tool-witnesses.md#conditional-title-and-tool-routes) and [archive-supplied instructions](../../research/game-tool-witnesses.md#archive-supplied-routing-instructions) supplement the compiled handler and wizard matrix. [Archive batch and audit effects](../../research/wizard-action-ledger.md#archive-batches-and-plugin-audit) distinguish scan, profile changes, generated deletion and partial archive publication. The [52-definition snapshot](../../research/first-run-catalog.md#effective-route-checks) has 364 rule rows; M3-G02 and M4-Y01/Y02/Y04 use shared fixture mechanics with per-definition expected destinations, not one pass per family.
- **Traceability:** the [capability register](../../inventory.md#capability-register) assigns outcomes and checks; the [packet catalog](2026-11-20-m2-m4-parity-packets.md) assigns owners. Fixture definitions, check commands and exact implementation files are still prospective where there is no baseline.
- **Open gates, not assumed exclusions:** named native filesystem/power-loss and private-view proofs; installed artifact availability, scope, updates, associations and runtime helpers; provider eligibility, keyring, diagnostic sharing, codec/format/redistribution rights; and any separate operation to abandon recorded deployment metadata. See the [decision and proof register](../../architecture.md#decision-and-proof-register) and [deferred outcomes](../../backlog-deferred.md). No production or live proof follows from this handoff.

### Before the first behavior dispatch

Do not confuse the full audit checkpoint C0, packet readiness, and runtime proof. M1-A00 is verified; that does not approve M1-C01 or establish any product behavior. Do not dispatch production code in the planning session.

| Gate | Missing evidence or permission | What it blocks |
|---|---|---|
| Baseline | [M1-A00 is verified](2026-11-20-m1-working-core.md#m1-a00--baseline-workspace-and-native-ci) on Linux and Windows. | Closed for the starting workspace only, not product or filesystem behavior. |
| First behavior brief | Approve the [M1-C01 packet](2026-11-21-m1-c01-resolver.md) and [contract](../../contracts/m1-file-resolution.md). In the same execution session, an oracle author turns the named witnesses into tests and an independent reviewer checks them before production dispatch. | First resolver implementation dispatch, not remaining source research. |
| Execution model preflight | Resolve configured IDs for the three selected models and verify `high` is available for each. Confirm independent review and ledger/resume in a manually started session before trusting the supervisor. | Unattended execution; never silently substitute. |
| Native scratch environments | Provide isolated ext4 and Btrfs Linux environments and native Windows NTFS. This checkout is on Btrfs; QEMU, `virsh` and `mkfs.ntfs` were not found on its `PATH`, which does not establish that a separate runner is unavailable. Destructive power-cut testing requires a separate disposable VM or disk image and approval. | Native probing and durability proof on an unavailable target; not pure resolver/archive checks. |
| C0 decision and review | Review the source handoff and affected fixture definitions; rule on consequential decisions or explicitly gate them. Live/native/package checks still require their own approvals and capabilities. | Full C0 sign-off and affected downstream packet freeze, not independent preparation. |

### Executor handoff and interruption

Each packet begins from an explicit revision/worktree or predecessor artifact, after independent checks confirm the ledger matches the actual diff and test state. The orchestrator records each subtask’s worker, owned paths, test assertions and evidence; at a checkpoint record reviewer findings, completed outputs, remaining commands, blocking decision and safe resume action. Run verification from the right isolated workspace. Do not repeat completed work unless evidence identifies an invalid result.

The existing supervisor is not the authority for product verification. For the first approved packet prefer a manually started session, confirm one complete run and one interrupted/resumed run, then decide whether unattended restart adds value. The supervisor currently assumes Git, an installed named agent and a specific ledger. Propose no extra MCP server or new runner until a real checkpoint exposes a missing capability. GitHub MCP is useful for remote evidence; Context7 for current libraries; the actual filesystem, packaged app and OS test matrix remain the verification source.
