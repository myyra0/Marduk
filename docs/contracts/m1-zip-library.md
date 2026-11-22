# M1-C02: ZIP admission and immutable library publication

**Status:** Proposed worker contract, not implemented. [M1-C02](../superpowers/plans/2026-11-20-m1-working-core.md#m1-c02--local-library-zip-admission-and-metadata) owns the archive and library modules. [Archive research](../research/archive-crosswalk.md) covers later formats.

## What we do differently

Use one in-process reader for this slice, no extractor fallback chain, filename repair, or direct write into a game directory. Keep each revision in a private library directory already named for its allocated ID. It is invisible to clients until one SQLite transaction publishes the revision and profile membership. There is no directory rename to a public mod-name path, and replacing an installed mod never rewrites its old bytes. Retain all published revisions in M1; safe eviction is not part of the first slice.

Candidate dependencies are frozen in the workspace manifests (`crates/archive/Cargo.toml`, `crates/storage/Cargo.toml`, `Cargo.lock`), which are the authority for exact versions and features; do not restate pins here. Both declare MIT; compiler and toolchain expectations are decided by `rust-toolchain.toml` and the lockfile, not restated here. The manifests and APIs were inspected; resolve the exact advisory check separately.

## Archive interface

```rust
pub struct ZipLimits {
    pub max_archive_bytes: u64,
    pub max_directory_bytes: u64,
    pub max_entries: usize,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
}

pub struct ZipEntry {
    pub path: RelativePath,
    pub uncompressed_len: u64,
}

pub struct AdmittedZip<R> { /* private reader and admitted entry descriptors */ }

pub fn admit_zip<R: Read + Seek>(
    input: R,
    limits: ZipLimits,
) -> Result<AdmittedZip<R>, ArchiveError>;

impl<R: Read + Seek> AdmittedZip<R> {
    pub fn entries(&self) -> &[ZipEntry];
    pub fn copy_entry(
        &mut self,
        index: usize,
        out: &mut impl Write,
    ) -> Result<u64, ArchiveError>;
}
```

`RelativePath` is the [C01 type](m1-file-resolution.md). The reader sees a seekable **private snapshot** of the selected archive, not a pathname reopened between preview and Apply. Storage owns snapshotting, actual writes and content digests. The archive layer exposes only validated regular-file entries by manifest index in original central-directory order. No raw offset or caller-selected path is trusted when streaming.

- Proposed M1 fixture bounds: 5 GiB input, 16 MiB central directory, 10,000 records (including directory entries), 1 GiB per expanded file, 4 GiB total expanded regular-file bytes. Use checked arithmetic. Limits can be lowered by tests. The streaming side enforces declared length and bounds independently of header claims; a checksum or length error invalidates the entire stage.
- Start with single-disk classic ZIP, stored or deflated, without prepended/trailing data. ZIP64, unusual comments/extra fields and other format variants are not first-slice promises. Before relying on reader metadata allocation, prove that the selected adapter enforces the central-directory size and record count before allocation. If it cannot, prefer a narrow classic-ZIP pre-admission check validated against the selected reader's actual directory, or replace the reader. Do not forward unbounded metadata into the reader or put a bound only on the returned list.
- Validate raw filename bytes before using any decoded name, preserve spelling and reject NUL, non-portable paths, ambiguous decoded names and type inconsistencies; do not use mangled paths. Reject duplicate exact names **before trusting the indexed manifest** (`ZipArchive::has_duplicate_names` in this version); reject case-equivalent names, mixed spellings of the same canonical directory and file/directory overlaps across the admitted entries. Optional explicit directory entries may name parents but do not themselves become installed content.
- Reject encrypted entries, archive-provided links and special objects, and unsupported compression before staging any payload. Never apply archive permissions or executable metadata in M1. Finish streaming through EOF and check integrity; on failure the storage module abandons the stage, not just the failed entry. A later compatibility expansion can normalize names only before collision checking.
- `ArchiveError` reports a bounded stable reason: `InvalidContainer`, `UnsupportedContainer`, `DuplicateRawName`, `UnsupportedEntry { index }`, `UnsafeName { index }`, `ConflictingNames { first, second }`, `LimitExceeded { kind }`, `CorruptEntry { index }`, `MissingEntry { index }`, or `Io`. Admission indices are central-directory record positions for entries observable by the reader; an unindexed exact duplicate uses `DuplicateRawName`. Streaming indices are positions in `entries()`. `LimitKind` is `ArchiveBytes`, `DirectoryBytes`, `Entries`, `FileBytes`, or `TotalBytes`. Raw untrusted names and paths are not default log fields.

## Library interface and state

```rust
pub struct BeginImport {
    pub profile: ProfileId,
    pub expected_revision: ProfileRevision,
    pub target: ImportTarget,
}

pub enum ImportTarget {
    Append { display_name: String },
    Replace { member: MemberId },
}

pub struct PreparedImport {
    pub id: ImportId,
    pub profile: ProfileId,
    pub expected_revision: ProfileRevision,
    pub archive_sha256: [u8; 32],
    pub files: Vec<TargetFile>,
}

// on the storage adapter
pub fn prepare_import(
    &mut self,
    input: impl Read,
    plan: BeginImport,
) -> Result<PreparedImport, ImportError>;
pub fn publish_import(&mut self, id: ImportId) -> Result<ProfileSnapshot, ImportError>;
pub fn discard_import(&mut self, id: ImportId) -> Result<(), ImportError>;
```

`ImportId` is opaque; the durable row binds it to the immutable archive fingerprint, target, and staged revision. The caller submits an explicitly selected file stream and later the reviewed ID, never a staging pathname or mutable manifest. Normalize the display name by trimming; require nonempty printable text and at most 120 UTF-8 bytes; names are labels, not directory components. Replacing a member preserves its ID, position, enabled flag and display name. Appending creates an enabled member at the end (highest priority). Do not infer replacement from a display name.

1. Use one exclusive library writer guard during staging, publication and reconciliation, not across time waiting for the user to Apply. Guard authority is the active OS lock, not PID, file existence or age. Distinguish an active writer and a lost/incomplete stage on restart.
2. Reserve an ID and an associated private revision path before creating that path. The library controls the root, exclusive creation and all writes; unexpected namespace/metadata is a refusal, never an invitation to delete it. Copy and hash the input to a private archive snapshot within limits, admit it, then write only approved files to the private payload root. Mark `Prepared` only after payload files and required namespace evidence are durable and manifest/digests are recorded.
3. `publish_import` first checks under the guard whether the ID already committed. On replay it returns that recorded post-commit snapshot without repeating the mutation; the client can separately read the current profile. Otherwise recheck `Prepared`, stage integrity, expected revision and target. Use one SQLite `IMMEDIATE` transaction to publish the installed revision and update membership/profile revision; keep the result bound to the import ID. `StalePlan` leaves profile, published revisions and original files unchanged; an explicitly discarded ID cannot publish.
4. Reconcile an interrupted `Writing` reservation only when no writer owns it; do not present it as prepared. Retain durable `Prepared` entries for explicit Apply/discard. Discard only an identified unpublished owned stage, and clean orphaned snapshot files through the same guarded reconciliation. Never remove any stage if a committed catalog entry references it. On a failed or ambiguous commit, reopen and consult the recorded state before declaring either success or removable data.
5. Store migrations in the storage adapter. Verify `foreign_keys=ON`, `journal_mode=WAL` and `synchronous=FULL` for the local durable writer. This is not SQLite committing external files. The platform's private-directory persistence proof is a dependency of C02 crash guarantees; missing evidence blocks those guarantees, not pure ZIP tests.

Freeze only the schema used by C02: profiles and revisions, ordered members, immutable installed revisions and manifests, and import reservations/results. M1-R02 adds generations, pre-image references and operation records in its own migration. Use one local app-owned catalog (`catalog.sqlite3`) and a verified library root with an owned `revisions/<id>/` namespace for private admission and published payloads. Reserve the ID before creating its private directory; catalog publication changes visibility, not the payload path. An archive snapshot is private import data, not a published download. Store portable root/relative references; IDs, labels and user paths never substitute for a current verified root. M2 may add separate owned profile storage scopes without moving or overwriting immutable payload; defer download, private-view and tool paths to their owners. Target-side temporaries still belong to the destination's proven rename domain, not this library layout.

`ImportError` operational distinctions: `InvalidName`, `StalePlan`, `MemberNotFound`, `Busy`, `UnknownImport`, `ImportNotPrepared`, `StageChanged`, `Archive(ArchiveError)`, `Storage`. Return no partial new profile as success. Registry/index constraints enforce unique IDs and association of each committed member with one published revision.

## Planner witnesses

| Test | Exact distinction to prove |
|---|---|
| `zip_exact_duplicate_is_not_a_last_wins_entry` | Fixture with two exact raw names with different contents fails even if the reader indexes one name; no payload writer is reached. Create the fixture separately if the writer API rejects duplicates. |
| `zip_alias_aborts_stage` | `Textures/Winner.dds` and `textures/winner.dds` fail; single valid `textures/winner.dds = b\n` streams exact bytes. Include file/directory overlap and two differently spelled canonical parents. |
| `zip_untrusted_entry_never_creates_output` | Negative fixtures for unsafe names, link/special mode, wrong compression or encryption. Have a parent/outside sentinel and confirm no publication or side effect. |
| `zip_rejects_unbounded_or_corrupt_payload` | A lowered expansion cap, oversized directory declaration, truncated data and bad checksum are errors; snapshot/payload stage never becomes `Prepared`. |
| `failed_reinstall_keeps_previous_revision_and_bytes` | Interrupt a B replacement of existing A before metadata commit; reopen and verify A's identity/bytes and old revision. No new visible B. |
| `stale_install_preview_publishes_nothing` | Prepare at revision 8, change the profile to 9, then Apply the old ID. Get `StalePlan` before publishing; no membership update, no public B. |
| `published_import_replay_is_idempotent` | Close after commit and before reply, reopen, submit the same ID. Return the recorded resulting snapshot/revision, without a second member or increment. |
| `reconciliation_never_reclaims_referenced_payload` | Create an abandoned `Writing` reservation, one durable prepared stage, and one published revision. Startup cleanup can account for the first, retains the second as pending, and cannot remove the third. |

Future check commands: `cargo test -p marduk-archive --test admission` and `cargo test -p marduk-storage --test library_contract`. The prepared fault model is not permission to claim disk durability on unverified targets.
