# Task 5 native effect oracle freeze

Authority: Task 5 brief; architecture freeze §C/E; M1-R01 scoped metadata ruling (2026-11-22); M1-R02/R03; Task 6 oracle plus its current additive recovery seams. Synthetic proof only. No native production deployment adapter, real game writes or power-cut qualification.

## Proof entry point

`marduk_platform::native_proof` is a hidden/non-frontend synthetic proof surface, not a permission or production dispatcher. Native engine calls must be the same candidates measured by the proof, not test-only fake effects. The harness creates an isolated synthetic installation and passes only its owned private area and library. `ProofSession` does not implement `Journal`; Task 6 owns the existing journal, phase/replay/CAS and `unexpected_displacements`. No changes to portable types or frozen tests required.

```rust
pub struct ProofSession { /* private */ }
#[derive(Debug)]
pub enum Checkpoint {
    BeforeNativeEffect,
    NativeRename { from: RootIdentity, to: RootIdentity },
    AfterNativeEffect { raw_error: Option<i32> },
}
pub enum Fault { None, LoseAcknowledgement }
impl ProofSession {
    pub fn open(
        location: &Path, layout: &GameLayout, installation: &RootIdentity,
        data: &RootIdentity, private: &Path, library: &Path,
        checkpoint: Box<dyn FnMut(Checkpoint) -> Fault>,
    ) -> Result<Self, RootError>;
    pub fn stage_path(&self, at: StageRef) -> PathBuf;
    pub fn backup_path(&self, at: BackupRef) -> PathBuf;
}
impl CopyEffects for ProofSession { /* exact existing signatures */ }
impl DeploymentInputs for ProofSession { /* exact existing signatures */ }
```

Diagnostic paths are read-only proof witnesses, not authorization inputs. Typed refs resolve only within the bound operation namespace. On Linux exchange, the physical former stage is the canonical retain alias once it contains a displaced artifact; logical `StageRef` is then absent. `stage_path` must expose the stage witness, not claim the aliased displaced file is the staged output; if it returns the physical alias, the test checks `Artifact::Absent` for the logical stage and checks the physical bytes via `backup_path`. `AlternateDisplaced` is detection-only: reobserve it even on restart, never publish there. A non-absent alternate or mismatched canonical retain is evidence for Task 6 to block settlement.

`BeforeNativeEffect` is a deterministic race at the last native boundary, after normal core/port preflight and before native effects. It does not grant exclusion to the implementation: recheck/handle pin/exclusion may prevent or refuse the race, but the callback must run in a qualifying case. `AfterNativeEffect` captures the actual native return immediately, including errno/Win32 code when applicable; no native error claimed when none was returned. `LoseAcknowledgement` drops a successful return before interpretation and exercises the same reobservation as a late error. It is injected ambiguity, not a fabricated OS code or a replacement for genuine refusal fixtures. `Fault::None` is the only normal behavior. Unsupported safety boundaries remain blockers, not no-op implementations returning success.

`NativeRename` traces the source and destination parent identities immediately before every native rename-family syscall, including private preparation, so a failed cross-domain attempt followed by copying cannot pass the Btrfs case. The independent raw `EXDEV` control is outside the port.

## Session ownership and outcomes

- The proof session keeps a live `InstallationBinding`; `verify_root`/`verify_recovery_root` reopen and compare the exact recorded installation and Data pair, including Btrfs tree/view identity and recovery's journal locator, without adopting a lookalike. For each observation, stage, barrier and effect, re-resolve `BoundTarget.target.relative` through that binding, compare exact parent identity and leaf spelling, and keep the authority handles live through the native call. Strings, an old handle by itself, a digest, or an inode number alone do not prove containment. An escaped-parent write is a failing witness even if later reported as conflict. An enforceably pinned parent may block substitution without forcing the legitimate publish to fail; report the actual native refusal.
- `DeploymentInputs` owns leases with the root gate before library gate; `resolve_target` yields the checked parent/leaf. `profile_revision` and `source` read authoritative state under the appropriate lease. For the synthetic proof, source revision 1 is seeded separately from the game; copy verified immutable source bytes locally, not the revision directory or source entry. A future production composition reads catalog revision and verified immutable file record, not a caller path. No bridge to public deployment commands in Task 5.
- `prepare` rechecks eligibility, creates durable separate generation-original and current-step backup claims for an original regular file, stages a checked output in the target rename domain, and reserves the absent canonical retain slot. `validate_backup` checks slot identity and ownership plus length/digest and supported metadata independently of the original public identity. `StepOutput::Original` selects the recorded Restore/undo backup; staging it preserves the backup and matches bytes and scoped metadata. The installed Restore output must match its staged identity; the *actual original object*, if displaced, keeps its own identity in the canonical slot. Do not discard either recovery claim on a failed/conflicted Restore.
- No-replace create/detach, exchange or qualified replacement, with no cross-domain rename/link or copy/delete fallback. `Observed` only from exact reobservation. `Displaced { at: step.retain, .. }` reports actual retained unknown bytes/identity and blocks a clean settle. `Conflict` can represent an exact clean refusal without displacement. `Unavailable` is no native effect or exact unchanged state proved; unknown/mixed states are ambiguous or conflicting, never assumed unchanged. Backups, stage and canonical retention are scanned independently of syscall status and never auto-deleted on doubt.
- Linux candidate: preflush files and private namespace, directory-relative `RENAME_NOREPLACE`/`RENAME_EXCHANGE`, then flush affected file objects and both changed parent dirfds and any newly created ancestors; reobserve before `Durable`. Linux post-validation ancestor/kind races are not solved by dirfds alone. Windows candidates: `MoveFileExW` without replace/copy flags and `ReplaceFileW` with absent same-volume backup and no merge-ignore flags; prove native handle/share containment and scoped post-evidence. Regular-file `FlushFileBuffers`, a SQLite WAL receipt and successful process kill are not NTFS namespace or power evidence. Keep any unqualified recipe unavailable.
- Scoped metadata only: Linux regular single-link, bytes, mode/UID/GID/mtime, qualified flags, successful empty visible xattrs. Windows ordinary single-link, owner/group/DACL with protection/inheritance, allowed attributes/last-write, successful EA/stream and selected `READ_CONTROL` queries; explicit label/resource/scope ACE, EA and named stream refuse before staging. Record hidden classes as unobserved. Any failed/partial probe or later discrepancy refuses or blocks; never broaden privileges or substitute a content-only promise.

## Private library bridge

Keep §C's `platform::library_stage::NativeReceipt` opaque, with `evidence(&self) -> &StageEvidence`. Storage's private `StagePort` consumes only the concrete native issuance; `Catalog` remains the public entry, no public test injection and no reverse platform→storage dependency. Stage under the live matching `WriterGuard`: allocate the durable `Writing` ID, record the owned revision-directory identity, stream a bounded private ZIP snapshot, admit/stream its members from that snapshot and independently hash exact payload files. Record allocation/revision, owner, archive digest and ordered file length/digests. Flush regular files and all relevant independently verified new entries before returning a receipt and recording `Prepared`. SQL is not proof that a payload entry exists.

`publish_import(id)` reobserves the persisted allocation/owner, archive and exact payload tree against the live native adapter before SQL publishes immutable membership. Never reopen the selected ZIP. `discard_import`/reconcile cannot remove changed or unverifiable data by inference. `StageChanged` means a proved mismatch; `StageUnavailable` means unsupported or unprovable evidence. No visible revision for either. Already-committed ID returns the recorded result first, without a second stage/publish or revision increment; post-commit ambiguous SQL ACK requires authoritative reread. Unqualified configurations keep the concrete bridge disconnected. The storage witness links the real `Catalog` from the locked workspace; its prepared/committed cut markers are external client protocol points, not SQL or power-failure injection.

## Per-case expected outcomes

| Witness | Required result |
|---|---|
| `native_no_replace_honors_concurrent_create` | Child proves it reached the absent-path race and created `edit\n`. Port reports conflict; actual child bytes survive at target and immutable stage remains. |
| `native_replace_records_actually_displaced_file` | Actual edit is observed independently by file ID, metadata and bytes; either clean refusal with stage intact and edit still public, or exact edit retained at canonical ref with typed displacement and independent original/step backups of `base\n`. |
| `native_rejects_root_and_parent_substitution` | Substitute root cannot bind to recorded pair. At native cut, symlink/junction redirection or moving a pinned parent outside Data must not change outside or moved-original entries. A proved sharing refusal of relocation means only that route was pinned. |
| `native_detach_preserves_unknown_file` | Same race as replace, no output stage; edit remains public or is the same real object in canonical quarantine, never deleted or settled as expected removal. |
| `native_error_does_not_prove_no_change` | Lost acknowledgement after a real move is ambiguous with actual new target and retained original; inspect exact unchanged/mixed namespace on genuine sharing, missing, metadata and filesystem refusals. No generic claims for 1175/1176/1177 unless actually witnessed. |
| `native_namespace_survives_claimed_barrier` | Before publish: target absent. After publication/before barrier: process observes the new name but makes no durability claim. After a claimed barrier: reopened verifier sees the same new object and bytes. Process-only; power not run. |
| `library_catalog_never_names_missing_payload` | Interrupted stream and prepared cut never list the payload; intact prepared receipt is recoverable and unlisted. Changed/missing stage cannot publish; changed payload also cannot be discarded. After committed cut, exact original snapshot bytes are present despite selected ZIP rewrite; same ID replays without publishing again. |
| `btrfs_cross_subvolume_copy_does_not_use_cross_subvolume_rename` | Direct cross-subvolume no-replace rename returns `EXDEV`, both endpoints reobserved. Stage is in target tree; same-tree replace and Restore preserve scoped evidence. Matching substitute root inode in another tree cannot inherit authority. Owned scratch teardown refusal is not run, never a shell or fake filesystem substitute. |

## Research

- GitHub code search and [systemd `fs-util.c`](https://github.com/systemd/systemd/blob/cac05a9eb3ffa7b81b65c5b7d4a11d23c6098696/src/basic/fs-util.c) (LGPL-2.1-or-later): dir-relative `RENAME_NOREPLACE` is an existing pattern; its link/unlink and racy compatibility fallbacks violate this contract. No source copied.
- [Linux rename(2)](https://man7.org/linux/man-pages/man2/rename.2.html) and [fsync(2)](https://man7.org/linux/man-pages/man2/fsync.2.html): cross-mount refusal, exchange object-kind limitations, and file versus containing-directory durability. Use native Btrfs subvolume commands and identity, not same-device inference.
- Context7 `/microsoft/windows-rs`, locked `windows-sys` 0.61.2 source and Microsoft [ReplaceFileW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-replacefilew), [MoveFileExW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw), [FlushFileBuffers](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-flushfilebuffers): signatures, failed-replace partial namespace/metadata changes, and file versus namespace barriers. Vendor docs override snippets. See [metadata decision research](metadata-scope-research.md) for the scoped security probe sources. Research does not qualify a configuration.

## Authoring verification

Native integration and workspace tests stop at deferred red (`E0432`: `marduk_platform::native_proof` not implemented). The tmpfs runner links the real catalog probe and reaches the same compile gate. The Windows-target check reaches that gate too; this is not NTFS execution. The existing storage library contract test (5 tests), workspace build and formatting pass. No R03 witness executed; no runtime evidence is claimed for local Btrfs, tmpfs or ext4, hosted NTFS, or power cuts.
