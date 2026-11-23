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
| `crates/platform/src/effects.rs` | Qualified native publication primitives: publish, exchange/replace, detach and copy. Uses the R01 root interface and R03 findings |
| `crates/storage/src/native_effects.rs` | Session adapter binding the journal to those primitives: live root pair, operation-owned slots, evidence and recovery. Uses the R01 root interface and R03 findings |

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

## Availability is a recorded fact

Which `(capability, operating system, volume class)` configurations may run is decided in one place: the qualification record in `crates/core/src/qualification.rs`. Each proof names the witness tests, the date and the revision that produced it, and the rung it reached: **primitives** (file-level effects ran), **staging** (the private staging adapter ran), **adapter** (the composed native adapter ran in the proof lane), **catalog** (publication through the production catalog ran), **composition** (the whole workspace service — production catalog, journal and adapter — previewed, applied and restored through the proof lane), and **deploy** (a client-visible deploy or restore ran). A client deploy or restore route requires the composition rung: the client-visible rung is recorded after the first real run, so it cannot be a precondition of the gate that enables it, and crash/power-cut evidence is deferred rather than required. `crates/core/tests/availability_guard.rs` parses the evidence matrices in `docs/research/m1-native-results.md` and fails when the record and the document disagree in either direction, so availability cannot move without a run behind it. Gates query the record; platform code reports only the observed volume class and whether its code exists for that operating system. Every configuration without a composition-rung proof stays unproven for deploy and restore.

## Phases and effect boundaries

1. **Planning/preflight:** validate identities, paths, source digests and expected targets. Refuse mixed existing directory aliases, unsupported metadata or missing parents. An existing active deployment is `ActiveDeployment` in M1; only Restore may change it. If recovery evidence is unresolved, do not write that root.
2. **Prepare the whole operation:** reserve operation ID and acquire the exclusive per-root cross-client lease. Validate again. For each intent, save a durable pre-image or durable record of absence and ready-to-publish staged bytes, then make every intent and its expected pre/post evidence durable. A durable pending row by itself is not authority to touch a destination with a changed identity. No public write before this barrier.
3. **Apply:** deploy in canonical target order. Restore walks the committed intents in reverse order. Use checked native placement or detachment from the public name. Preserve actually displaced bytes until the observation is reconciled. WAL stores `Applied` per step only after the platform returns the required durability evidence. If a step may have changed the filesystem don't skip it just because the ACK is absent.
4. **Finalize:** one durable database transition sets the active generation plus completed operation, or clears the generation with a completed restore. For M1, cancellation is supported only before opening the public-write phase. An ambiguous commit result is not proof the row rolled back; reopen and inspect it. A lost reply after durable finalize returns the existing outcome for the same plan, without another effect.
5. **Recover:** under the same lease, classify each pending intent's observed target as matching the pre-state, the intended post-state, unknown or unavailable. For unfinished deploy, undo in reverse order. For unfinished restore, finish the remaining work in restore order. Preserve all necessary backups until durable success; record an unavailable or unknown state without wiping evidence. Uncommitted deploy rollback does not create an active generation. Unfinished restore retains its active-generation reference until durable completion. Two recoveries in the same already-resolved state are no-ops.

### Fake-model ledger and lease

- Recovery checks the target, stage, retain slot, and required backups together. A Deploy step that did not publish records `Undone` with barrier-confirmed pre-state evidence and no undo intent. An executed undo records `Undone` against its own staged fingerprint and retained evidence. Restore only retries a complete pre-state and rechecks all settled steps immediately before finalization.
- The catalog records independent digests of the approved plan (profile, revision, exact root pair and ordered intents) and the sealed forward intents, including empty sets. Sealing and terminal transactions validate exact intent coverage, phase, evidence, and reference ownership. Status, replay and recovery use the same checked decoder. Rows with missing or older intent-only proofs fail closed; approval is not reconstructed from the rows it would protect.
- Acquire the root gate, then the catalog's `library.lock` for profile/source validation, then the separate `journal.lock` for journal writes. Journal code never acquires the library gate. Both use one SQLite catalog and `schema.rs` remains the schema owner. Release the library gate once the public phase is recorded; the root lease remains in force.
- Setup cannot deselect a pair with a pending operation or active generation, nor move its locator. `Journal::verify_pending_pair` binds recovery to the recorded lease pair and locator before `DeploymentInputs::verify_recovery_root` runs. The default methods are for synthetic ports; a native port must reopen that recorded locator. `Journal::block_displacement` records the reported reference and observation, exposes only operation-owned references in status, and `has_unresolved_displacement` prevents automatic retries or terminal release until review. The default journal methods preserve compatibility with in-memory fakes; the SQLite journal implements the durable checks.
- Copy effects may retain a displaced file only in the sealed retain slot (a physical exchange alias may make that the stage slot); the deterministic alternate slot is a detection slot, never a valid publication destination. Original and step backups are not displacement destinations. Both locations must be inspectable before an effect and after restart; `inspect_alternate_displacement` defaults to absent only for ports that never write there. A non-empty alternate or a canonical retain that does not match the expected displaced artifact is recorded even without an effect ACK and blocks a clean settle. Status lists its checked reference with the distinct `alternate` slot; `OperationJournal::unexpected_displacements` returns the corresponding observations. The `alternate` wire location reports evidence only and is never offered as a publication destination. A canonical retain reported as retained must have been positively observed; an unreadable slot is not advertised as known.

### Interrupted exchange and displaced slots

On Windows an exchange is two no-replace renames (target → `displaced`, stage → target), so a process
can stop between them. The recoverable intermediate shape is exact: the target is absent, the stage
holds the sealed post-image, and the canonical `displaced` slot (or the physical stage alias on Linux)
holds the expected pre-image. Recovery treats that shape as a first-class state rather than a conflict:

- A pending **deploy** step in `Attempting` is reverted: `displaced` moves back to the target with no
  replace, the stage is left untouched, and `Undone` is recorded only after the target matches the
  recorded pre-image.
- A pending **restore** step in `Attempting` (forward or undo) is completed: the stage moves onto the
  absent target, then the displaced object is retained, and `Applied`/`Undone` is recorded only after
  the post-state fingerprint matches.
- Timestamp authority is bound to both objects before any move and never reopened by path afterwards.
  An occupied target is never overwritten; any other slot shape remains a typed refusal.

`windows_exchange_process_cut` in `crates/workspace/tests/native_deployment.rs` produces that state
through the production effect path in one process and recovers it in a separate process on NTFS; the
recorded result is Deploy `rolled_back` and Restore `completed`. Linux does not have this state: its
exchange is one `renameat2`.

A foreign object that ends up in the physical stage slot (the accepted final-race window) is reported
with `slot: "stage"`, latches `RecoveryConflict`, and keeps its bytes; status names the slot so the
operator can find the displaced file. The latch survives reopen until an explicit review clears it.

### Import reservations and startup reclaim

A `writing` import row owns `revisions/<id>` from before the first archive byte, and the adapter records
the native directory identity before staging begins. `Catalog::reconcile` therefore reclaims a killed or
refused import only from proven ownership: a recorded owner removes the whole proved revision tree; an
unowned empty directory is removed; unowned non-empty bytes, a changed or unavailable stage, and any
published import are quarantined and left in place. Prepared and committed data is never reclaimed. The
workspace calls this exactly once on open and reports the result as the status reconciliation described
in [M1-U01](m1-clients.md); it is not called again per poll.

### Native seam that R03 must qualify

The platform effect must support **observe**, **publish new into an absent path without clobber**, **exchange/replace existing while retaining the actually displaced file**, and **detach an expected existing file without destroying unknown bytes**. R03 must specify the exact Rust port signature and map the native return and reobserved state to outcomes for each qualified configuration. Do not replace it with check-then-overwrite or check-then-delete.

- Immediately before each effect, verify the expected pre-state through live handles, including the recorded root identity, parent binding and leaf state. An earlier path string or directory scan is not evidence. Refuse the effect if the pre-state cannot be verified.
- Reobserve after every attempted effect, including one that returns an error. A successful syscall alone does not establish success; an error does not establish that nothing changed. Divergence from the required state yields `Ambiguous`, `Conflict` or `Unavailable`, never `Observed`.
- Pre-image and replacement data must be made stable before a public change; then qualify a namespace persistence barrier before calling a step durable. Record failure at either barrier. Linux qualifies `fsync` of the file and its parent directory. Windows uses the narrowed route in [ADR 0004](../adr/0004-windows-writes-use-standard-flush.md): the same flush-then-reobserve sequence, a step is `Durable` only when the reobserved evidence is unchanged, and a power cut is not covered. A narrowed barrier never counts as crash evidence.
- If a checked change displaced different bytes, preserve the actually displaced file as well as the original backup. Only put it back if a guarded operation proves it will not clobber a newer edit; otherwise report where it was retained. Do not finalize a clean deployment with a discrepancy.
- If a step expected absence but sees an unknown newly created file, do not delete or replace it. If the root is detached or replaced, report `TargetUnavailable`, not absence, and never reroute a retry to a different install.

**Known limitation (accepted by the 2026-11-22 user ruling):** On Linux and Windows, a same-privilege process can substitute the parent or leaf between the final live-handle check and the native syscall. Marduk does not claim to prevent that substitution. It mitigates the race by checking for divergence after the effect, retaining actually displaced bytes, and refusing to retry automatically on divergence. On Windows the effect primitives move by pathname: the identity-proven target-parent handle stays open with read and write shared and delete sharing refused for the whole effect, and the parent identity is re-proved immediately before the call, but the kernel still resolves the pathname itself. The shared write is what lets the kernel open the destination inside the pinned parent; denying delete sharing is what keeps the bound name from being renamed or replaced. A substitution of an ancestor between that re-proof and the syscall is the same accepted final window. Windows deploy is available from the recorded composition run; the narrowed barrier in [ADR 0004](../adr/0004-windows-writes-use-standard-flush.md) still excludes power-loss recovery from the claim.

### Catalog file containment (app-owned data root)

The catalog is app-owned state, not a game install, so containment follows the same ruling: a same-privilege substitution of the application data directory is accepted as a documented limitation, not a defect to solve with a custom SQLite VFS.

- **Handle-verified:** the application data root is opened as a pinned directory descriptor (`openat2` with no symlink or magic-link resolution on Linux; a reparse-tag-free pinned handle on Windows) and re-proved by native identity before the first write, after SQLite opens, and after schema preparation. The `catalog.sqlite3` leaf is opened or created through that descriptor and its inode/volume identity is re-proved by `PrivateFile` around the same points. On Windows the catalog handle is held for the catalog's lifetime with `FILE_SHARE_READ | FILE_SHARE_WRITE` and no `FILE_SHARE_DELETE`, so the OS refuses a rename or replacement of the leaf while it is open.
- **Name-resolved:** SQLite receives a pathname (Linux: `/proc/self/fd/<dirfd>/catalog.sqlite3`; Windows: the pinned leaf's final path) and resolves it itself. It derives `catalog.sqlite3-wal`, `catalog.sqlite3-shm` and `catalog.sqlite3-journal` from the resolved spelling and opens those by name. No handle of ours is consulted for those opens, and rollback journals are created and unlinked by name during the connection's life.
- **Residual:** on Linux the locked SQLite Unix VFS does not retain the directory descriptor. `unixFullPathname` (`appendAllPathElements`) resolves `/proc/self/fd/<dirfd>` by `readlink` and rewrites the path to the pinned directory's pathname before the file is opened, so the main file and every sidecar are opened by name. Two windows follow. **Main file:** after that resolution, a same-privilege process can rename the pinned directory and place a replacement `catalog.sqlite3` (or a symlink to one) at the resolved path before SQLite opens it. SQLite is opened without `SQLITE_OPEN_NOFOLLOW`, so the open may follow a redirected leaf; `PrivateFile::verify` and `verify_root_path` run after the open and after schema preparation, detect the changed leaf or directory identity, and refuse to continue without claiming a write (`journal_reopen_refuses_a_catalog_replaced_by_a_link` and `journal_reopen_refuses_a_substituted_app_root` in `crates/storage/tests/journal_boundaries.rs`). **Sidecars:** `catalog.sqlite3-wal`, `-shm` and `-journal` are derived from the same resolved spelling and opened or created by name later in the connection's life, after those re-proofs, so a substitution or planted sidecar there is not examined by the leaf or directory checks; a connection can attach to it. Pinning the leaf and sidecars would require a custom SQLite VFS that opens every file relative to the held descriptor; that is not implemented. Both windows require write access to the user's own application data directory, which is also direct access to the catalog. Marduk detects the main-file substitution at the post-open re-proof and refuses to continue; it does not claim to prevent or detect a sidecar substitution.

### Private library staging on Windows

Windows private staging is admitted by its own predicate: `library_stage::fsops::qualified` opens the root and requires the volume that owns the handle to be NTFS. The workspace-level `library_volume_qualified` gate follows the recorded catalog-layer NTFS proof, so catalog imports and deploys are connected on Windows.

- **Handle-relative:** the library root, `revisions/` and every bound revision are opened as no-follow directory handles that share read and write but deny delete sharing, so the OS refuses to rename or replace them while a stage is bound; each name is still re-proved against the live handle and its recorded pathname. Revision entries and every payload child are created, opened, listed and removed with `NtCreateFile` or `SetFileInformationByHandle`, with `OBJECT_ATTRIBUTES.RootDirectory` set to a held handle. Payload and archive-entry operations are handle-relative; bookkeeping re-opens (`reverify_revisions`, `verify_revision_path`, `sync_dir`) are pathname-based and re-proved immediately. `FILE_OPEN_REPARSE_POINT` plus `OBJ_DONT_REPARSE` refuse links. Identity is volume serial plus `FILE_ID_INFO` file id; it is re-proved on every re-observe against the live handle and the recorded pathname.
- **Privacy:** revision directories are created with a self-relative security descriptor, a protected DACL, the token's default owner, a full-control ACE for that owner and an inherit-only ACE for descendants. Re-observation refuses an owner change or any ACE naming another SID, so a `0700`-equivalent revision cannot be reopened after its access widens.
- **Durability:** payload and archive handles are flushed with `FlushFileBuffers`; the receipt is issued only after the bound `revisions/` handle and, on reclaim, the bound root handle are flushed.
- **Residual:** the root, `revisions/` and each bound revision directory cannot be renamed or replaced while their handles are live, but a same-privilege process can still add, delete or rewrite files *inside* a revision between the last re-proof and receipt issuance. Marduk detects that at the next re-observe and returns `StageChanged` or `StageUnavailable`; it does not claim to prevent the redirected access inside that window. This is the same accepted final-window substitution as the Linux staging sidecar and the catalog limitation above.


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
