# M3-I02: interactive installer implementation plan

**Status:** Draft, not dispatchable. **Goal:** Preserve installer choices, review and replay them safely, and publish only the reviewed output to the bound profile.

**Architecture:** Archive admission returns a private stage and opaque entry IDs. Core evaluates choices and plans a manifest from those entries. Storage persists drafts and uses the existing library publication protocol. An installer does not execute archived code, write directly into a game or bypass ordinary publication.

**Tech stack:** Rust; M2-I01 archive admission and M1/M2 library/profile operations. **Spec:** [integration contracts](../research/integration-contracts.md#interactive-installation--m3-i02-m4-y04), [archive trace](../research/archive-crosswalk.md), [effect trace](../research/headless-crosswalk.md#resource-and-effect-trace-downloads-and-installers).

## Shared boundary

| Operation | Inputs and result |
|---|---|
| `open_installer` | Admitted archive identity, instance/profile and exact revision → `InstallerSessionId`, `DraftRevision`, visible steps/options, validation, or `UnsupportedInstaller`. Does not publish a mod. |
| `get_installer` / `update_installer` | Get by session ID; updates bind the exact revision and visible option ID plus desired selected state, not a relative toggle by row number. Return recomputed visible choices and validation. Stale or hidden option attempts have no effects. |
| `resume_installer` | Saved draft ID → reopen the exact admitted archive, parser contract and instance/profile context, re-evaluate saved selections, produce a fresh revision for review or report the changed input. No old Apply approval survives re-open. |
| `preview_installer` / `apply_installer` | Preview consumes session + revision and reviewed naming/collision choices → `InstallPlanId` with entry-to-target mapping. Apply claims that exact plan → existing or new `OperationId`; it cannot silently swap profile, source revision or overwrite policy. |
| `close_installer` | Close an idle session or explicit saved draft. Removes only owned draft/staging resources after checking in-flight references; cannot cancel or erase a claimed publication. Transport disconnect may end interactive waiting, not commit or discard a saved draft. |
| `answer_installer_request` | For a collection or running install, bind `OperationId + RequestId + PromptRevision` to the typed answer. Accepting an answer advances that workflow, not an unrelated standalone draft. A cancelled or superseded prompt is non-writable. |

The durable binding includes archive digest, parser contract version, instance identity, profile revision and the relevant dependency snapshot (installed/enabled content and file predicates). Persist stable selections and input identifiers, not a trusted temporary directory or raw UI state. Recreate the stage where needed and verify the digest. Even a byte-identical re-extraction cannot reuse an old Apply approval after reopening. Values such as `MissingInput`, `StaleDraft`, `OptionUnavailable`, `UnsupportedInstaller`, `Busy` and `TargetCollision` survive transport as structured outcomes.

The pinned saved/reopen path is **parsed XML FOMOD**, not scripted FOMOD or every format. Keep XML FOMOD, BAIN package selection, and declared independent multi-mod installation as distinct adapters. Do not require saved/reopen support for BAIN solely because it shares a dialog. A mutually exclusive bundle selects content for one mod; a multi-mod archive selects independent child mods. Preserve sequential, independently committed children for the latter, as observed in the reference. Improve reporting: a failure in child 2 leaves child 1 explicitly committed and child 3 unstarted. Never silently render that as overall success or rerun child 1 after reconnect. A failed child preserves its own pre-existing revision.

## Worker boundaries and proposed commands

1. **Planner and session state.** `crates/core/src/installer.rs`, `crates/core/tests/installer_sessions.rs`; consume M2-I01 `ArchiveEntryId`/manifest and profile contract. Freeze exact Rust signatures and parser limits after the predecessor. Core owns allowed transitions and draft approval, not extraction or parser dependencies.
2. **Adapters.** `crates/archive/src/installers/{mod,fomod,bain,bundle,multi_mod}.rs` plus `crates/archive/tests/installer_formats.rs`. Evaluate layout/conditions against the approved inputs, return entry and target IDs; use no shell, active game directory or direct filesystem publication. A relative path in an option or preview image must resolve within its admitted stage; decode through the bounded preview interface rather than returning arbitrary file paths. Freeze exact admitted XML, BAIN variants, the generic bundle/child-selection interface, and image format/budget before assigning adapter work. Use a declared synthetic layout here; title-specific route detection belongs to M4-Y01, setup tools and executable patches to M4-Y04.
3. **Draft/replay storage.** `crates/storage/src/installer_drafts.rs` and `crates/storage/tests/installer_drafts.rs`; freeze migration ordinal against the base artifact. Persist exact inputs and selections; storage never bypasses stale checks or regenerates a missing mod from ambient profile state.
4. **Shared client flow.** Reuse M1-U01 operation status, exact revision, and trusted picker rules. Freeze client ownership paths before dispatch; one desktop/browser witness for selecting and re-opening FOMOD, shared headless witnesses in M4-A02. Failure text must distinguish invalid choices, unavailable files, and inputs that changed.

## Planner witnesses

| Planner witness | Fixture and assertion |
|---|---|
| `fomod_selection_changes_only_reviewed_output` | Admitted `base.dds = base\n`, `a.dds = a\n`, `b.dds = b\n`; base always maps to `textures/base.dds`, one selected option maps to `textures/choice.dds`. Select B: result is exactly `base\n` and `b\n`; hidden or stale A update preserves draft and publishes nothing. |
| `reopened_fomod_rechecks_context_and_requires_new_apply` | Same bytes, parser and profile context reopen with B selected and fresh revision. Old plan cannot start a new publication. Reopen with same filename but changed archive digest, parser contract, or relevant profile state returns a typed mismatch, keeps the prior saved evidence, and publishes nothing. |
| `bain_selection_vets_packages_without_fomod_replay` | With `00 Base/textures/base.dds = base\n` and `01 Optional/textures/bonus.dds = bonus\n`, base-only omits bonus; selecting both includes exactly both files after package stripping. The FOMOD-only saved draft route must not reinterpret this archive as XML. |
| `exclusive_bundle_is_one_mod_not_two_children` | Two exclusive declared variants each supply `textures/choice.dds`, with `a\n` and `b\n` respectively. B alone gives one mod and `b\n`; selecting both is invalid and produces no child publications. Title-specific archive detection is a separate check. |
| `independent_children_report_partial_publication` | Review children A, B, C in order, inject B failure pre-commit; report A committed, B failed with its prior revision unchanged, C unstarted. Reconnect returns the same outcome. A separate fixture with two children aimed at the same logical mod destination is refused at preview, before A's publication. |
| `close_or_disconnect_does_not_publish` | Open a draft; disconnect at a prompt and verify no active library revision changes. Reopen the saved draft; cancel before publication or close it. Neither action deletes the admitted archive or sibling drafts. Replay an already committed operation from another client without publishing again. |
| `preview_image_cannot_escape_admitted_stage` | Normal visible preview decodes within the fixed front-end budget. A relative escape, hidden-option request, or over-limit image returns an unavailable preview; no outside sentinel is read and no staged payload changes. |

Future checks: `cargo test -p marduk-core --test installer_sessions`, `cargo test -p marduk-archive --test installer_formats`, and `cargo test -p marduk-storage --test installer_drafts`. Planner supplies the fixed inputs and tests at the contract freeze; workers add tests without editing the provided assertions. Use only synthetic archives. These witnesses do not stand in for the full format/variant audit; do not freeze an interpreter from one fixture alone.
