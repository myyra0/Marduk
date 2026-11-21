# High-risk integration contracts for the parity plan

**Status:** Planning decisions and test design, not implementation. These contracts constrain the M3/M4 packets; they do not freeze provider wire fields that still need evidence. See [inventory](../inventory.md) and [execution packets](../superpowers/plans/2026-11-20-m2-m4-parity-packets.md).

## Downloads and versions — M3-S01/S02/S03, M4-S04

- **Identity:** use a provider-specific logical identity with provider, game/community, package/mod and exact file/version as applicable. Keep the artifact content digest separate. An expiring URL or an archive filename is neither identity nor authorization. Resolve one approved object to an acquisition descriptor, not an arbitrary desktop URL.
- **Partial state:** persist desired identity, expected limits/checksum when supplied, validator and received verified length. Resume only if the server confirms the matching object and exact expected range; if it returns a full response, restart into a new staging file. Checkpoint progress does not imply bytes have passed final verification. Hash or length mismatch leaves no active archive or install.
- **Trust:** credentials remain in keyring; provider use, redirect policy, rate limits and login mechanisms are adapter-specific. Resume and errors may not log auth headers, session parameters or restricted URLs. Acquire and provider mock tests do not need live credentials.
- **Install and rollback:** `preview` binds exact installed revision and archived identity; changing the selected game, file or install revision makes it stale. Retaining a version keeps the original payload recoverable without redownloading “latest.” If it was explicitly cleaned up, report the action needed; do not claim rollback succeeded.

**Oracles:** Same URL with changed validator and same filename with another provider ID; range mismatch; partial resume across app restart; bad final checksum; expired provider entitlement; previous archive missing; one batch member fails without falsely completing the others.

## Interactive installation — M3-I02, M4-Y04

- Archive admission yields a staged manifest of allowed entries with verified bytes; layout planning refers to manifest entry IDs and authorized target roots. Staged archive paths or choices are not permission to execute bundled programs.
- Read, select, answer and apply are separate operations. Key a session by archive digest, installer parser contract, game/instance/profile revisions, and an incrementing draft revision. Revalidate visible choices and the produced manifest when applying; changing input invalidates approval.
- XML FOMOD choices/conditions, BAIN package selections, multi-mod ownership and game-specific prompts use the same review/apply shape but distinct interpreters. No new scripting runtime for unsupported scripted FOMOD. Persist selections rather than opaque mutated extraction state; resumed sessions recreate/validate the required stage before commit.
- Multiple independent mods in one archive are independent recorded publications under a reviewed parent operation. If failure yields a supported partial outcome, enumerate exactly what's committed and what remains; otherwise roll back the group. That policy is part of the approved brief and cannot differ silently by client.

**Oracles:** A hidden choice is rejected, a changed archive cannot reopen under its old ID, a cancel does not activate content, and one selected multi-mod branch cannot collide with another and replace its files in place. All selected bytes must be derivable from the admitted manifest.

## Collections and lists — M4-S01/S02/S03

- Separate immutable **input package** (source descriptor and digest), **reviewed plan** (selected artifacts, options, destinations, costs and external prerequisites), **job evidence** (resolved inputs, durable step state, reviews), and **installed set** (ownership and adopted user content). Never identify a set only by its mutable title. An incomplete set is not an installed success.
- For collection revisions, record what is imported, already present/adopted, created by this set or user-supplied. Adoption does not imply permission to remove or overwrite. Keep authored/default activation, profile membership, file priority and game-content plugin order distinct. Preview options, update policy, bundle/diff/path changes and manual-download handoff.
- Wabbajack starts with the pinned manifest's nine directive families: `FromArchive`, `PatchedFromArchive`, `MergedPatch`, `InlineFile`, `RemappedInlineFile`, `PropertyFile`, `ArchiveMeta`, `CreateBSA`, `TransformedTexture`. Validate references, topological dependencies and output target identities before materializing. An unhandled required directive blocks apply with evidence instead of silently producing an incomplete setup. Verify output bytes/digests for each implemented directive and track stock-game version input separately.
- Start collections and Wabbajack as callers of the approved archive, game and providers modules, not special paths around them. A publish/upload is a separate consent point with immutable package identity; never try it as a hidden side effect of saving a draft or CI.

**Oracles:** User pre-existing mod alongside a new owned mod: removing the set removes only its ownership, not the user's mod. On interruption, resume revalidates sources, draft revision and outputs rather than assuming completed filenames are current. Wrong game version, mismatched input hashes, replaced download, unsatisfied manual prompt or unsupported directive never enters active state.

## Game handlers and external tools — M4-Y01/Y02/Y03/Y04

- Use narrow, typed `GameDefinition` data plus explicitly registered behavior where data is insufficient. A handler publishes desired roots and transformations; platform owns opened paths, file operations, launch intents and recovery. One family can share fixtures only if it has the same behavior, not just the same engine name.
- Per handler/tool, record install identity, OS and launcher route, file type/layout intent, mod load mechanism, writable root set, supported version, ownership of generated files, rollback boundary and verification scope. Different executable paths, package constraints and plugin rules must appear as variants.
- Tool acquisition, package validation, optional install, and explicit run are distinct. A reviewed descriptor identifies the exact artifact/variant and allowed file changes. Tools independently acting outside declared runs cannot be automatically transactional; don't claim control over broad side effects. Authorized live workflows use throwaway profiles even when the application can avoid mutating real user data.
- Native launch failures are not reasons to silently fall back to different files from a more permissive view. Supported routes must include route-specific tool output isolation and safe failure. A game-specific integrity refusal or unsupported launcher stays visible.

**Oracles:** Fail after publishing one of two roots, launch with wrong edition/actual executable, change game build after approval, edit a generated file during restore, fail external dependency install, and test a tool can't adopt another profile's captures. No third-party manager code is required.

## Headless and desktop integration — M4-A02/A03

Use one domain operation with explicit modes for read, preview and apply. Inputs that can change state attach a revision, reviewed effect or platform-authorized target as applicable. An operation ID supports status across restart; caller disconnect is not proof it canceled. Keep bounded notifications separate from durable job state. App command capabilities and the stdio transport each limit which operations can be reached; neither layer replaces validation inside the shared module.

**Oracles:** Repeat an accepted request across reconnect, cancel at the last safe point, lose progress updates, submit against a previous revision, and compare final state from both clients against the same fixture and operation record. Neither a path received over stdio nor a desktop selection string is used as unconditional write authority.
