# M1-R02: physical-copy deployment and recovery contract

**Status:** Proposed logical contract. M1-R03 must qualify the OS primitives and error states before R02 is implementation-ready. M1-R04 provides the later runtime proof; this document is not that proof. See the [golden fixture and failure oracle](../research/recovery-oracle.md) and [ADR 0002](../adr/0002-journal-external-effects-before-publication.md).

## Keep the first slice small

One existing `Data` root, one physical deployment generation, copy only. A first deployment may add files inside existing parent directories and replace regular files; it does not create target directories. If a parent is missing, the reviewed plan refuses before writes. Directory creation/cleanup and applying a second generation without restoring first belong to M2-D01, not to the instructions for Beam implementing M1. No extra roots, link creation, elevated privileges, game-specific post-effects or automatic “try a different filesystem” fallback.

This is simpler than trying to define one journal for every link, overlay and game operation before proving any of them. The core sees checked intents and outcomes, not native API names. Expanding the presented mode or roots later still uses these ownership, durability and conflict rules.

## Ownership and stable types

| File | Owns |
|---|---|
| `crates/core/src/deployment.rs` | Preview data, ordered intents, profile/root/revision validation and operation status |
| `crates/core/src/recovery.rs` | Decisions when stepping through an operation or reconciling persisted phases and observations |
| `crates/storage/src/operation_journal.rs`, `migrations/0002_operations.sql` | Pending root claim, intents, durable phase transitions, active-generation association and retained evidence |
| `crates/platform/src/physical_copy.rs` | Qualified staged file, observed identity, native publication and retention of changed/displaced bytes. Uses the R01 root interface and R03 findings |

`DeploymentPlan` has `id: PlanId`, `profile: ProfileId`, `revision: ProfileRevision`, `installation: RootIdentity`, `data: RootIdentity`, and a canonical ordered `Vec<FileIntent>`. An intent binds `target: TargetFile`, the approved source `(InstalledRevisionId, RelativePath)`, the source digest and the expected original observation. `RootIdentity` is an opaque adapter-proof token for the recorded installation and opened data root, not a path string.

`Fingerprint` contains a digest, native file identity token and the supported metadata witness. Platform evidence specifies the exact fields and how identity survives or changes in allowed native calls. Comparing only names, timestamps or digests is not enough to authorize a destructive action. `BackupRef` and `StageRef` are opaque references into operation-owned storage. They never resolve a caller-supplied arbitrary path. A recorded existing original has a `BackupRef`; a recorded absent original has no backup.

Expose these operations from the shared core service, with typed wrappers in CLI/Tauri:

| Operation | Inputs | Output and rules |
|---|---|---|
| `preview_deployment` | Selected installation/data roots and exact profile revision | `DeploymentPlan`, or refusal without a game write. Inspect destinations, files, parents, and active deployment ownership. |
| `commit_deployment` | Exact reviewed `PlanId` | `OperationId` and observable status; one durable claim for the plan. Cannot apply with a different profile, source, target root or input after preview. |
| `restore` | Exact installation/data identity | A tracked restore against the committed generation, or a typed `NoActiveDeployment` no-op. Never derive recovery targets from current profile contents. |
| `operation_status` | `OperationId` | Authoritative state, action required and retained locations when known. A vanished caller is not cancellation. |
| `recover_pending` | Recorded installation/data identity with the exclusive root lease | Reconcile the one pending operation for the root; report status or a typed blocker and leave other roots alone. |

The operation owner validates a new plan under the root lease before the first write. For an already claimed plan ID, return the recorded operation before evaluating current active-generation or stale-profile checks; replay is not a fresh deployment. Use stable codes for `StalePlan`, `ActiveDeployment`, `Busy`, `TargetUnavailable`, `UnsupportedTarget`, `InvalidPath`, `RecoveryConflict`, and `Io`. Native detail may be included only in safely redacted diagnostics. Keep false success and ambiguous late failure separate.

## Phases and effect boundaries

1. **Planning/preflight:** validate identities, paths, source digests and expected targets. Refuse mixed existing directory aliases, unsupported metadata or missing parents. An existing active deployment is `ActiveDeployment` in M1; only Restore may change it. If recovery evidence is unresolved, do not write that root.
2. **Prepare the whole operation:** reserve operation ID and acquire the exclusive per-root cross-client lease. Validate again. For each intent, save a durable pre-image or durable record of absence and ready-to-publish staged bytes, then make every intent and its expected pre/post evidence durable. A durable pending row by itself is not authority to touch a destination with a changed identity. No public write before this barrier.
3. **Apply:** deploy in canonical target order. Restore walks the committed intents in reverse order. Use checked native placement or detachment from the public name. Preserve actually displaced bytes until the observation is reconciled. WAL stores `Applied` per step only after the platform returns the required durability evidence. If a step may have changed the filesystem don't skip it just because the ACK is absent.
4. **Finalize:** one durable database transition sets the active generation plus completed operation, or clears the generation with a completed restore. For M1, cancellation is supported only before opening the public-write phase. An ambiguous commit result is not proof the row rolled back; reopen and inspect it. A lost reply after durable finalize returns the existing outcome for the same plan, without another effect.
5. **Recover:** under the same lease, classify each pending intent's observed target as matching the pre-state, the intended post-state, unknown or unavailable. For unfinished deploy, undo in reverse order. For unfinished restore, finish the remaining work in restore order. Preserve all necessary backups until durable success; record an unavailable or unknown state without wiping evidence. Uncommitted deploy rollback does not create an active generation. Unfinished restore retains its active-generation reference until durable completion. Two recoveries in the same already-resolved state are no-ops.

### Native seam that R03 must qualify

The platform effect must support **observe**, **publish new into an absent path without clobber**, **exchange/replace existing while retaining the displaced file**, and **detach an expected existing file without destroying unknown bytes**. This is a small set of operations with explicit old/new/unknown outcomes, not a guarantee that every OS can express the same primitive. R03 must produce the exact Rust port signature and mapping from native return plus directory state to these outcomes, including safe behavior when an arbitrary external writer races. Do not replace it with check-then-overwrite or check-then-delete.

- Pre-image and replacement data must be made stable before a public change; then qualify a namespace persistence barrier before calling a step durable. Record failure at either barrier. Root identity and containment must be proved through reopened handles/operations—not an earlier path string or directory scan.
- If a checked change displaced different bytes, preserve the actually displaced file as well as the original backup. Only put it back if a guarded operation proves it will not clobber a newer edit; otherwise report where it was retained. Do not finalize a clean deployment with a discrepancy.
- If a step expected absence but sees an unknown newly created file, do not delete or replace it. If the root is detached or replaced, report `TargetUnavailable`, not absence, and never reroute a retry to a different install.

## Planner witnesses and gates

Use `Data/textures/winner.dds = base\n`, `Data/untouched.txt = keep\n` and optional `Data/outside-plan.txt = user\n`. The candidate plan creates `a-only.dds = only-a\n` and replaces `winner.dds` with `b\n`. The canonical publish order is `a-only.dds`, then `winner.dds`; reverse it for Restore.

| Future test | Exact distinguishing result |
|---|---|
| `cut_after_record_before_publish` | Restart after durable intents but before any public write. Original `base\n` remains, `a-only.dds` absent, no active generation. |
| `cut_after_publish_before_ack` | Cut after replacing `winner.dds` but before recording its step, with `a-only.dds` already applied. On restart any allowed pre/post state ends at `base\n`, absent `a-only.dds`, no active generation. |
| `failed_final_barrier_no_success` | Return platform persistence failure after a public effect. No committed success; on recovery reconcile the result from durable evidence, not the error text. |
| `committed_before_reply_does_not_reapply` | Durable commit followed by loss of response. Reopen and replay the plan ID: same status/generation, `b\n`, `only-a\n`, no extra write. |
| `restore_twice_is_noop` | Cut restore after publishing `base\n` and before detaching `a-only.dds`; recovery removes the unchanged managed file, retains both sentinels, and a second restore changes nothing. |
| `restart_preserves_external_edit_and_backup` | An external `edit\n` at the target is not replaced; original `base\n` remains recoverable, repeat observes `RecoveryConflict`, and no write is attempted on that target on retry. |
| `wrong_volume_waits` | On a valid-looking path to a different root, recover reports `TargetUnavailable` without reading/writing that root; restore evidence remains retained. |
| `unknown_external_file_is_not_removed` | External file at a recorded absent/then-managed path cannot be unlinked by matching a pathname or a new file with the same bytes. Preserve it and block automatic resolution. |
| `missing_parent_is_refused_before_effects` | Only existing parents are in M1. Reject the whole plan without creating earlier unrelated planned files or directories. |

`cargo test -p marduk-core --test recovery` is the logical/fault-model check. R03 independently verifies native effects and storage namespace persistence; R04 cuts real child processes and performs the approved VM power-off runs on named ext4, Btrfs and native NTFS configurations. Neither a fake model nor another configuration can satisfy those gates. Keep unfinished evidence visible rather than lowering the recovery requirement.
