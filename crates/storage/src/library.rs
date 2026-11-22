//! SQLite-backed immutable library catalog.
//!
//! One local catalog owns profiles, installed revisions, staged imports and
//! recorded publication results. Payload bytes stay under `revisions/<id>/`;
//! the catalog changes visibility, never payload paths.

use std::collections::{BTreeMap, BTreeSet};
#[cfg(test)]
use std::fs;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

#[cfg(test)]
use std::fs::OpenOptions;
#[cfg(test)]
use std::io::Write as _;

#[cfg(test)]
use marduk_archive::{M1_ZIP_LIMITS, admit_zip};
use marduk_core::ids::{InstalledRevisionId, MemberId, ProfileId, ProfileRevision};
#[cfg(test)]
use marduk_core::import::ArchiveError;
use marduk_core::import::{
    BeginImport, ImportError, ImportId, ImportTarget, NamedProfile, PreparedImport,
    ProfileEditError,
};
use marduk_core::import::{ImportAllocation, PayloadFile, StageEvidence};
use marduk_core::profiles::{ProfileMember, ProfileSnapshot};
use marduk_core::resolve::ResolvedInputs;
#[cfg(test)]
use marduk_core::root_identity::{NativeIdentity, RootIdentity};
use marduk_core::targets::RelativePath;
use marduk_core::targets::{RootRole, TargetFile};
use marduk_platform::library_stage::NativeStage;
use marduk_platform::{GateError, WriterGate, WriterGuard};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};
#[cfg(test)]
use sha2::{Digest, Sha256};

#[cfg(test)]
mod boundary_tests;
#[cfg(test)]
mod contract_tests;
#[cfg(test)]
mod stage_tests;

const REVISIONS_DIR: &str = "revisions";
#[cfg(test)]
const PAYLOAD_DIR: &str = "payload";
#[cfg(test)]
const ARCHIVE_FILE: &str = "archive.zip";

pub struct Catalog {
    root: PathBuf,
    /// Pins the verified app-owned directory for this catalog's lifetime.
    _root_handle: File,
    /// Pins the catalog leaf so a later reopen cannot substitute it.
    _catalog: marduk_platform::PrivateFile,
    connection: Connection,
    gate: WriterGate,
    stage: StageAdapter,
}

impl Catalog {
    /// Open the local catalog, creating its root and `revisions/` directory.
    ///
    /// Configuration and migration run under the library writer gate. PRAGMAs
    /// are read back from the live connection and a mismatch is an error.
    pub fn open(root: &Path) -> Result<Self, ImportError> {
        // Prove the app-owned root before creating anything beneath it. The
        // handle refuses a final symlink/junction, so a redirect cannot turn a
        // later `revisions/`, lock or SQLite write into a game-tree write.
        let root_handle =
            marduk_platform::open_private_directory(root).map_err(|_| ImportError::Storage)?;
        marduk_platform::open_private_child_directory(&root_handle, REVISIONS_DIR)
            .map_err(|_| ImportError::Storage)?;

        let gate =
            WriterGate::open_at(&root_handle, crate::schema::LOCK_FILE).map_err(gate_error)?;
        let _guard = gate.try_lock().map_err(gate_error)?;

        let catalog =
            marduk_platform::open_or_create_private_file(&root_handle, crate::schema::CATALOG_FILE)
                .map_err(|_| ImportError::Storage)?;
        let catalog_path = catalog
            .path(&root_handle)
            .map_err(|_| ImportError::Storage)?;
        // The pathname starts at the pinned directory descriptor, so our own
        // opens follow no substituted ancestor. SQLite resolves the leaf and
        // sidecars itself; the substitution residual is owned by
        // `docs/contracts/m1-recovery.md` (catalog file containment).
        let mut connection = Connection::open_with_flags(&catalog_path, OpenFlags::default())
            .map_err(storage_error)?;
        catalog
            .verify(&root_handle)
            .map_err(|_| ImportError::Storage)?;
        // The SQLite pathname cannot consume the descriptor, so re-prove that
        // it still names the pinned directory after the open.
        marduk_platform::verify_private_directory(root, &root_handle)
            .map_err(|_| ImportError::Storage)?;
        crate::schema::prepare(&mut connection).map_err(|_| ImportError::Storage)?;
        catalog
            .verify(&root_handle)
            .map_err(|_| ImportError::Storage)?;
        marduk_platform::verify_private_directory(root, &root_handle)
            .map_err(|_| ImportError::Storage)?;

        Ok(Self {
            root: root.to_path_buf(),
            _root_handle: root_handle,
            _catalog: catalog,
            connection,
            gate,
            stage: Self::select_stage(marduk_platform::library_volume_qualified(root), root),
        })
    }

    /// Connect native staging only on volume classes with recorded runtime
    /// evidence (`docs/research/m1-native-results.md`). A qualified volume whose
    /// private boundary will not bind stays refused; the bind is the final gate,
    /// never a downgrade to a weaker write.
    fn select_stage(qualified: bool, root: &Path) -> StageAdapter {
        if !qualified {
            return StageAdapter::Unavailable(UnavailableStage);
        }
        match NativeStage::open(root) {
            Ok(stage) => StageAdapter::Native(stage),
            Err(_) => StageAdapter::Unavailable(UnavailableStage),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether this catalog can write a private staged payload at all.
    pub fn stage_available(&self) -> bool {
        self.stage.available()
    }

    /// Read the profile snapshot and its display names in one transaction.
    pub fn named_profile(&mut self, profile: ProfileId) -> Result<NamedProfile, ImportError> {
        let transaction = self.connection.transaction().map_err(storage_error)?;
        let snapshot = read_snapshot(&transaction, profile)?;
        let names = read_names(&transaction, profile)?;
        transaction.commit().map_err(storage_error)?;
        Ok(NamedProfile { snapshot, names })
    }

    pub fn snapshot(&mut self, profile: ProfileId) -> Result<ProfileSnapshot, ImportError> {
        Ok(self.named_profile(profile)?.snapshot)
    }

    /// Load the installed-revision manifests for one profile, in the shape the
    /// deployment resolver consumes. Membership is read from the same
    /// transaction as the files. A revision with no recorded files yields an
    /// empty manifest; a revision absent from the catalog is omitted, so the
    /// resolver reports `MissingManifest` for it.
    pub fn resolved_inputs(&mut self, profile: ProfileId) -> Result<ResolvedInputs, ImportError> {
        let transaction = self.connection.transaction().map_err(storage_error)?;
        let snapshot = read_snapshot(&transaction, profile)?;
        let mut manifests = BTreeMap::new();
        for member in snapshot.members {
            if manifests.contains_key(&member.installed) {
                continue;
            }
            let revision =
                i64::try_from(member.installed.get()).map_err(|_| ImportError::Storage)?;
            let exists: Option<i64> = transaction
                .query_row(
                    "SELECT 1 FROM installed_revisions WHERE id = ?1",
                    [revision],
                    |row| row.get(0),
                )
                .optional()
                .map_err(storage_error)?;
            // An unknown revision has no manifest at all, which the resolver
            // reports as `MissingManifest`. A published revision with no rows
            // is an empty manifest, not a missing one.
            if exists.is_none() {
                continue;
            }
            let paths = {
                let mut statement = transaction
                    .prepare(
                        "SELECT path FROM revision_files \
                         WHERE revision_id = ?1 AND role = 'data' ORDER BY ordinal ASC",
                    )
                    .map_err(storage_error)?;
                let rows = statement
                    .query_map([revision], |row| row.get::<_, String>(0))
                    .map_err(storage_error)?;
                rows.collect::<Result<Vec<_>, _>>().map_err(storage_error)?
            };
            let mut files = Vec::with_capacity(paths.len());
            for path in paths {
                let relative = RelativePath::parse(&path).map_err(|_| ImportError::Storage)?;
                files.push(TargetFile {
                    root: RootRole::Data,
                    relative,
                });
            }
            manifests.insert(member.installed, files);
        }
        transaction.commit().map_err(storage_error)?;
        Ok(ResolvedInputs { manifests })
    }

    pub fn prepare_import(
        &mut self,
        input: impl Read,
        plan: BeginImport,
    ) -> Result<PreparedImport, ImportError> {
        let target = match plan.target {
            ImportTarget::Append { display_name } => ImportTarget::Append {
                display_name: validate_label(&display_name)?,
            },
            ImportTarget::Replace { member } => ImportTarget::Replace { member },
        };

        let _guard = self.lock()?;
        let mut input = input;

        match &mut self.stage {
            StageAdapter::Unavailable(stage) => stage.prepare(),
            StageAdapter::Native(stage) => {
                let id = reserve_import(
                    &mut self.connection,
                    plan.profile,
                    plan.expected_revision,
                    &target,
                )?;
                let allocation = ImportAllocation {
                    id,
                    revision: InstalledRevisionId::new(id.get()),
                };
                let connection = &mut self.connection;
                let receipt = stage.stage(&_guard, allocation, &mut input, &mut |owner| {
                    let owner = crate::operation_journal::encode_root(owner);
                    let changed = connection
                        .execute(
                            "UPDATE imports SET owner = ?1 \
                             WHERE id = ?2 AND state = 'writing'",
                            params![owner.as_slice(), sql_import_id(id)?],
                        )
                        .map_err(storage_error)?;
                    if changed != 1 {
                        return Err(ImportError::Storage);
                    }
                    Ok(())
                })?;
                let evidence = receipt.evidence();
                persist_stage(
                    &mut self.connection,
                    id,
                    &evidence.archive_sha256,
                    &crate::operation_journal::encode_root(&evidence.owner),
                    &evidence.files,
                )?;
                Ok(PreparedImport {
                    id,
                    profile: plan.profile,
                    expected_revision: plan.expected_revision,
                    archive_sha256: evidence.archive_sha256,
                    files: evidence
                        .files
                        .iter()
                        .map(|file| file.target.clone())
                        .collect(),
                })
            }
            #[cfg(test)]
            StageAdapter::Fixture(stage) => stage.prepare(
                &mut self.connection,
                &self.root,
                &mut input,
                plan.profile,
                plan.expected_revision,
                target,
            ),
        }
    }

    pub fn publish_import(&mut self, id: ImportId) -> Result<ProfileSnapshot, ImportError> {
        let _guard = self.lock()?;
        let record = load_import(&self.connection, id)?;

        // Replay precedence: a committed ID reports its recorded result even
        // when the profile has moved on since.
        if record.state == ImportState::Committed {
            return read_result_snapshot(&self.connection, id);
        }
        if record.state != ImportState::Prepared {
            return Err(ImportError::ImportNotPrepared);
        }

        let stage = StageRecord {
            id,
            archive_sha256: record.archive_sha256,
            owner: record.owner.clone(),
            files: stored_files(&self.connection, id)?,
        };
        self.stage.reobserve(&_guard, &self.root, &stage)?;

        let revision = read_profile_revision(&self.connection, record.profile)?;
        if revision.get() != record.expected_revision.get() {
            return Err(ImportError::StalePlan);
        }
        if let ImportTarget::Replace { member } = record.target {
            ensure_member(&self.connection, record.profile, member)?;
        }

        match self.commit_publication(&record) {
            Ok(()) => read_result_snapshot(&self.connection, id),
            Err(error) => {
                // An ambiguous SQL outcome is inspected before reporting: if the
                // commit landed, the recorded result is authoritative.
                if load_import(&self.connection, id)
                    .is_ok_and(|record| record.state == ImportState::Committed)
                {
                    read_result_snapshot(&self.connection, id)
                } else {
                    Err(error)
                }
            }
        }
    }

    pub fn discard_import(&mut self, id: ImportId) -> Result<(), ImportError> {
        let _guard = self.lock()?;
        let record = load_import(&self.connection, id)?;
        match record.state {
            ImportState::Committed => return Err(ImportError::ImportNotPrepared),
            ImportState::Writing | ImportState::Prepared | ImportState::Discarded => {}
        }

        // Without a stage adapter there is no ownership receipt, so nothing can
        // be proven or removed; leave the row untouched.
        if !self.stage.available() {
            return Err(ImportError::StageUnavailable);
        }

        if record.state != ImportState::Discarded {
            // Tombstone before any reclamation so an interruption is retryable.
            let changed = self
                .connection
                .execute(
                    "UPDATE imports SET state = 'discarded' \
                     WHERE id = ?1 AND state IN ('writing', 'prepared')",
                    [sql_import_id(id)?],
                )
                .map_err(storage_error)?;
            if changed != 1 {
                return Err(ImportError::Storage);
            }
        }

        if is_published(&self.connection, id)? {
            return Err(ImportError::ImportNotPrepared);
        }

        let stage = StageRecord {
            id,
            archive_sha256: record.archive_sha256,
            owner: record.owner,
            files: stored_files(&self.connection, id)?,
        };
        self.stage.reclaim(&_guard, &self.root, &stage)
    }

    /// Verify prepared stages and account for abandoned reservations.
    ///
    /// Only stages that the private adapter re-observes intact are returned as
    /// pending. A changed or unavailable stage stays `prepared` in the catalog
    /// but is not presented as intact. Owned `writing` reservations are
    /// tombstoned before the adapter reclaims them; unowned ones remain
    /// quarantined. Committed and prepared bytes are never reclaimed here.
    pub fn reconcile(&mut self) -> Result<Vec<ImportId>, ImportError> {
        let _guard = self.lock()?;

        let prepared = import_ids_in_state(&self.connection, "prepared")?;
        let writing = import_ids_in_state(&self.connection, "writing")?;
        let discarded = import_ids_in_state(&self.connection, "discarded")?;

        // A tombstone may survive a crash or a failed reclaim. Retry cleanup
        // only for rows whose ownership receipt is intact; committed and
        // unprovable entries are never touched.
        for id in discarded {
            let record = load_import(&self.connection, id)?;
            if record.owner.is_none()
                || record.archive_sha256.is_none()
                || !self.stage.available()
                || is_published(&self.connection, id)?
            {
                continue;
            }
            let stage = StageRecord {
                id,
                archive_sha256: record.archive_sha256,
                owner: record.owner,
                files: stored_files(&self.connection, id)?,
            };
            match self.stage.reclaim(&_guard, &self.root, &stage) {
                Ok(()) | Err(ImportError::StageChanged) | Err(ImportError::StageUnavailable) => {}
                Err(error) => return Err(error),
            }
        }

        for id in writing {
            let record = load_import(&self.connection, id)?;
            if record.owner.is_none() || record.archive_sha256.is_none() {
                // No ownership receipt: quarantine rather than guess.
                continue;
            }
            if !self.stage.available() {
                continue;
            }

            let changed = self
                .connection
                .execute(
                    "UPDATE imports SET state = 'discarded' \
                     WHERE id = ?1 AND state = 'writing'",
                    [sql_import_id(id)?],
                )
                .map_err(storage_error)?;
            if changed != 1 {
                continue;
            }

            let stage = StageRecord {
                id,
                archive_sha256: record.archive_sha256,
                owner: record.owner,
                files: stored_files(&self.connection, id)?,
            };
            match self.stage.reclaim(&_guard, &self.root, &stage) {
                Ok(()) | Err(ImportError::StageChanged) | Err(ImportError::StageUnavailable) => {}
                Err(error) => return Err(error),
            }
        }

        let mut retained = Vec::with_capacity(prepared.len());
        for id in prepared {
            let record = load_import(&self.connection, id)?;
            let stage = StageRecord {
                id,
                archive_sha256: record.archive_sha256,
                owner: record.owner,
                files: stored_files(&self.connection, id)?,
            };
            match self.stage.reobserve(&_guard, &self.root, &stage) {
                Ok(()) => retained.push(id),
                Err(ImportError::StageChanged) | Err(ImportError::StageUnavailable) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(retained)
    }

    pub fn reorder(
        &mut self,
        profile: ProfileId,
        expected: ProfileRevision,
        order: &[MemberId],
    ) -> Result<ProfileSnapshot, ProfileEditError> {
        Ok(self.reorder_named(profile, expected, order)?.snapshot)
    }

    /// Reorder and return the snapshot with its display names from the same
    /// transaction, so callers never need a second read for the result.
    pub fn reorder_named(
        &mut self,
        profile: ProfileId,
        expected: ProfileRevision,
        order: &[MemberId],
    ) -> Result<NamedProfile, ProfileEditError> {
        let _guard = self.lock_edit()?;
        let profile_value = sql_profile_id(profile).map_err(|_| ProfileEditError::Storage)?;
        let expected_value =
            nonnegative_sql(expected.get()).map_err(|_| ProfileEditError::Storage)?;

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(edit_storage)?;

        let revision: Option<i64> = transaction
            .query_row(
                "SELECT revision FROM profiles WHERE id = ?1",
                [profile_value],
                |row| row.get(0),
            )
            .optional()
            .map_err(edit_storage)?;
        if revision != Some(expected_value) {
            return Err(match revision {
                Some(_) => ProfileEditError::StalePlan,
                None => ProfileEditError::Storage,
            });
        }

        let members =
            read_member_rows(&transaction, profile_value).map_err(|_| ProfileEditError::Storage)?;
        validate_permutation(&members, order)?;

        let next = expected
            .get()
            .checked_add(1)
            .ok_or(ProfileEditError::Storage)?;
        let next_value = nonnegative_sql(next).map_err(|_| ProfileEditError::Storage)?;

        if !members.is_empty() {
            // Move every ordinal into a disjoint temporary range so the unique
            // `(profile_id, ordinal)` constraint never sees a transient clash.
            let offset = members
                .iter()
                .map(|member| member.ordinal)
                .max()
                .and_then(|ordinal| ordinal.checked_add(1))
                .ok_or(ProfileEditError::Storage)?;
            transaction
                .execute(
                    "UPDATE members SET ordinal = ordinal + ?1 WHERE profile_id = ?2",
                    params![offset, profile_value],
                )
                .map_err(edit_storage)?;
        }

        for (ordinal, member) in order.iter().enumerate() {
            let ordinal_value = i64::try_from(ordinal).map_err(|_| ProfileEditError::Storage)?;
            let member_value =
                i64::try_from(member.get()).map_err(|_| ProfileEditError::InvalidOrder)?;
            transaction
                .execute(
                    "UPDATE members SET ordinal = ?1 WHERE id = ?2 AND profile_id = ?3",
                    params![ordinal_value, member_value, profile_value],
                )
                .map_err(edit_storage)?;
        }

        let changed = transaction
            .execute(
                "UPDATE profiles SET revision = ?1 WHERE id = ?2 AND revision = ?3",
                params![next_value, profile_value, expected_value],
            )
            .map_err(edit_storage)?;
        if changed != 1 {
            return Err(ProfileEditError::StalePlan);
        }
        let names = read_names(&transaction, profile).map_err(|_| ProfileEditError::Storage)?;
        transaction.commit().map_err(edit_storage)?;

        let by_id: BTreeMap<u64, &MemberRow> = members
            .iter()
            .map(|member| (member.id.get(), member))
            .collect();
        let reordered = order
            .iter()
            .map(|member| {
                let row = by_id
                    .get(&member.get())
                    .expect("permutation was validated above");
                ProfileMember {
                    id: *member,
                    installed: row.installed,
                    enabled: row.enabled,
                }
            })
            .collect();

        Ok(NamedProfile {
            snapshot: ProfileSnapshot {
                id: profile,
                revision: ProfileRevision::new(next),
                members: reordered,
            },
            names,
        })
    }

    /// Pin library revision and source files during deployment validation.
    /// Journal writes take a separate gate and can run inside this guard.
    pub fn lock(&self) -> Result<WriterGuard, ImportError> {
        self.gate.try_lock().map_err(gate_error)
    }

    fn lock_edit(&self) -> Result<WriterGuard, ProfileEditError> {
        self.gate.try_lock().map_err(|error| match error {
            GateError::Busy => ProfileEditError::Busy,
            GateError::Unavailable | GateError::Io => ProfileEditError::Storage,
        })
    }

    fn commit_publication(&mut self, record: &ImportRecord) -> Result<(), ImportError> {
        let import_value = sql_import_id(record.id)?;
        let profile_value = sql_profile_id(record.profile)?;
        let expected_value = nonnegative_sql(record.expected_revision.get())?;
        let next = record
            .expected_revision
            .get()
            .checked_add(1)
            .ok_or(ImportError::Storage)?;
        let next_value = nonnegative_sql(next)?;

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;

        transaction
            .execute(
                "INSERT INTO installed_revisions (id) VALUES (?1)",
                [import_value],
            )
            .map_err(storage_error)?;
        transaction
            .execute(
                "INSERT INTO revision_files (revision_id, ordinal, role, path, len, sha) \
                 SELECT import_id, ordinal, role, path, len, sha FROM staged_files \
                 WHERE import_id = ?1",
                [import_value],
            )
            .map_err(storage_error)?;

        match &record.target {
            ImportTarget::Append { display_name } => {
                let ordinal = next_ordinal(&transaction, profile_value)?;
                transaction
                    .execute(
                        "INSERT INTO members \
                         (profile_id, ordinal, installed_id, enabled, display_name) \
                         VALUES (?1, ?2, ?3, 1, ?4)",
                        params![profile_value, ordinal, import_value, display_name],
                    )
                    .map_err(storage_error)?;
            }
            ImportTarget::Replace { member } => {
                let member_value = sql_member_id(*member)?;
                let changed = transaction
                    .execute(
                        "UPDATE members SET installed_id = ?1 \
                         WHERE id = ?2 AND profile_id = ?3",
                        params![import_value, member_value, profile_value],
                    )
                    .map_err(storage_error)?;
                if changed != 1 {
                    return Err(ImportError::MemberNotFound);
                }
            }
        }

        transaction
            .execute(
                "INSERT INTO import_result_members \
                 (import_id, ordinal, member_id, installed_id, enabled, display_name) \
                 SELECT ?1, ordinal, id, installed_id, enabled, display_name \
                 FROM members WHERE profile_id = ?2",
                params![import_value, profile_value],
            )
            .map_err(storage_error)?;

        let changed = transaction
            .execute(
                "UPDATE profiles SET revision = ?1 WHERE id = ?2 AND revision = ?3",
                params![next_value, profile_value, expected_value],
            )
            .map_err(storage_error)?;
        if changed != 1 {
            return Err(ImportError::StalePlan);
        }

        let changed = transaction
            .execute(
                "UPDATE imports SET state = 'committed', result_revision = ?1 \
                 WHERE id = ?2 AND state = 'prepared'",
                params![next_value, import_value],
            )
            .map_err(storage_error)?;
        if changed != 1 {
            return Err(ImportError::Storage);
        }

        transaction.commit().map_err(storage_error)
    }

    /// Test-only constructor used by the frozen library contract oracle.
    #[cfg(test)]
    pub(crate) fn open_for_test(root: &Path) -> Result<Self, ImportError> {
        let mut catalog = Self::open(root)?;
        catalog.stage = StageAdapter::Fixture(FixtureStage);
        Ok(catalog)
    }

    /// Test-only constructor for the refused stage configuration: the same
    /// adapter an unqualified volume selects, with no test fake behind it.
    #[cfg(test)]
    pub(crate) fn open_unqualified_for_test(root: &Path) -> Result<Self, ImportError> {
        let mut catalog = Self::open(root)?;
        catalog.stage = Self::select_stage(false, root);
        Ok(catalog)
    }
}

struct StageRecord {
    id: ImportId,
    archive_sha256: Option<[u8; 32]>,
    owner: Option<Vec<u8>>,
    files: Vec<StoredFile>,
}

/// Private stage seam. There is no public constructor or product feature that
/// can substitute another adapter.
enum StageAdapter {
    Unavailable(UnavailableStage),
    Native(NativeStage),
    #[cfg(test)]
    Fixture(FixtureStage),
}

impl StageAdapter {
    fn available(&self) -> bool {
        match self {
            Self::Unavailable(_) => false,
            Self::Native(_) => true,
            #[cfg(test)]
            Self::Fixture(_) => true,
        }
    }

    fn reobserve(
        &mut self,
        guard: &WriterGuard,
        _root: &Path,
        stage: &StageRecord,
    ) -> Result<(), ImportError> {
        match self {
            Self::Unavailable(adapter) => adapter.reobserve(),
            Self::Native(adapter) => {
                let evidence = native_evidence(stage)?;
                adapter.reobserve(guard, &evidence)
            }
            #[cfg(test)]
            Self::Fixture(adapter) => adapter.reobserve(_root, stage),
        }
    }

    fn reclaim(
        &mut self,
        guard: &WriterGuard,
        _root: &Path,
        stage: &StageRecord,
    ) -> Result<(), ImportError> {
        match self {
            Self::Unavailable(adapter) => adapter.reclaim(),
            Self::Native(adapter) => {
                let evidence = native_evidence(stage)?;
                adapter.reclaim(guard, &evidence)
            }
            #[cfg(test)]
            Self::Fixture(adapter) => adapter.reclaim(_root, stage),
        }
    }
}

/// Refusal seam for unqualified volumes. Every prepare, re-observe and reclaim
/// is refused rather than faked, and no persisted stage can be treated as
/// intact.
struct UnavailableStage;

impl UnavailableStage {
    fn prepare(&mut self) -> Result<PreparedImport, ImportError> {
        Err(ImportError::StageUnavailable)
    }

    fn reobserve(&mut self) -> Result<(), ImportError> {
        Err(ImportError::StageUnavailable)
    }

    fn reclaim(&mut self) -> Result<(), ImportError> {
        Err(ImportError::StageUnavailable)
    }
}

/// Test-only fixture seam. Its owner record is synthetic and derived from the
/// staged archive bytes; it is not native evidence.
#[cfg(test)]
struct FixtureStage;

#[cfg(test)]
impl FixtureStage {
    fn prepare(
        &mut self,
        connection: &mut Connection,
        root: &Path,
        input: &mut dyn Read,
        profile: ProfileId,
        expected_revision: ProfileRevision,
        target: ImportTarget,
    ) -> Result<PreparedImport, ImportError> {
        let id = reserve_import(connection, profile, expected_revision, &target)?;
        let directory = revision_dir(root, id);
        fs::create_dir(&directory).map_err(|_| ImportError::Storage)?;

        let archive_sha256 = copy_archive(input, &directory.join(ARCHIVE_FILE))?;
        let owner = synthetic_owner(id, &archive_sha256);
        let owner_bytes = crate::operation_journal::encode_root(&owner);
        let changed = connection
            .execute(
                "UPDATE imports SET owner = ?1, archive_sha = ?2 \
                 WHERE id = ?3 AND state = 'writing'",
                params![
                    owner_bytes.as_slice(),
                    archive_sha256.as_slice(),
                    sql_import_id(id)?
                ],
            )
            .map_err(storage_error)?;
        if changed != 1 {
            return Err(ImportError::Storage);
        }

        let archive = File::open(directory.join(ARCHIVE_FILE))
            .map_err(|_| ImportError::Archive(ArchiveError::Io))?;
        let mut admitted = admit_zip(archive, M1_ZIP_LIMITS).map_err(ImportError::Archive)?;

        let mut payload_files = Vec::with_capacity(admitted.entries().len());
        for index in 0..admitted.entries().len() {
            let (relative, declared_len) = {
                let entry = &admitted.entries()[index];
                (entry.path.clone(), entry.uncompressed_len)
            };

            let path = payload_file(&directory, relative.as_str());
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|_| ImportError::Storage)?;
            }
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|_| ImportError::Storage)?;
            let mut writer = HashingWriter::new(file);
            let written = admitted
                .copy_entry(index, &mut writer)
                .map_err(ImportError::Archive)?;
            if written != declared_len {
                return Err(ImportError::StageChanged);
            }
            writer.flush().map_err(|_| ImportError::Storage)?;
            let (sha256, file) = writer.finish();
            file.sync_all().map_err(|_| ImportError::Storage)?;

            payload_files.push(PayloadFile {
                target: TargetFile {
                    root: RootRole::Data,
                    relative,
                },
                len: written,
                sha256,
            });
        }

        // Flush every new payload parent up through the library root. Native
        // directory durability on Windows remains Task 5.
        sync_parent_chain(&directory.join(ARCHIVE_FILE), root)?;
        for file in &payload_files {
            let path = payload_file(&directory, file.target.relative.as_str());
            sync_parent_chain(&path, root)?;
        }

        let evidence = StageEvidence {
            allocation: ImportAllocation {
                id,
                revision: InstalledRevisionId::new(id.get()),
            },
            archive_sha256,
            files: payload_files,
            owner,
        };

        let expected = evidence
            .files
            .iter()
            .map(|file| ExpectedFile {
                path: file.target.relative.as_str().to_owned(),
                len: file.len,
                sha: file.sha256,
            })
            .collect::<Vec<_>>();
        verify_files(&directory, &evidence.archive_sha256, &expected)?;

        persist_stage(
            connection,
            id,
            &evidence.archive_sha256,
            &owner_bytes,
            &evidence.files,
        )?;

        Ok(PreparedImport {
            id,
            profile,
            expected_revision,
            archive_sha256: evidence.archive_sha256,
            files: evidence.files.into_iter().map(|file| file.target).collect(),
        })
    }

    fn reobserve(&mut self, root: &Path, stage: &StageRecord) -> Result<(), ImportError> {
        let archive_sha256 = verified_owner(stage)?;
        verify_files(
            &revision_dir(root, stage.id),
            archive_sha256,
            &expected_files(&stage.files),
        )
    }

    fn reclaim(&mut self, root: &Path, stage: &StageRecord) -> Result<(), ImportError> {
        let directory = revision_dir(root, stage.id);
        if !directory.exists() {
            return Ok(());
        }

        verified_owner(stage)?;

        remove_file_if_present(&directory.join(ARCHIVE_FILE))?;
        for file in &stage.files {
            remove_file_if_present(&payload_file(&directory, file.path.as_str()))?;
        }
        prune_known_directories(&directory, &stage.files)?;
        if directory.exists() {
            return Err(ImportError::StageChanged);
        }
        Ok(())
    }
}

#[cfg(test)]
fn verified_owner(stage: &StageRecord) -> Result<&[u8; 32], ImportError> {
    let (archive_sha256, owner) = fixture_receipt(stage)?;
    let expected_owner =
        crate::operation_journal::encode_root(&synthetic_owner(stage.id, archive_sha256));
    if owner != expected_owner.as_slice() {
        return Err(ImportError::StageChanged);
    }
    Ok(archive_sha256)
}

fn validate_label(raw: &str) -> Result<String, ImportError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > 120 || trimmed.chars().any(char::is_control) {
        return Err(ImportError::InvalidName);
    }
    Ok(trimmed.to_owned())
}

fn reserve_import(
    connection: &mut Connection,
    profile: ProfileId,
    expected_revision: ProfileRevision,
    target: &ImportTarget,
) -> Result<ImportId, ImportError> {
    let profile_value = sql_profile_id(profile)?;
    let expected_value = nonnegative_sql(expected_revision.get())?;
    let (kind, display_name, member_id) = match target {
        ImportTarget::Append { display_name } => ("append", Some(display_name.as_str()), None),
        ImportTarget::Replace { member } => ("replace", None, Some(sql_member_id(*member)?)),
    };

    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage_error)?;

    let revision: Option<i64> = transaction
        .query_row(
            "SELECT revision FROM profiles WHERE id = ?1",
            [profile_value],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage_error)?;
    let revision = revision.ok_or(ImportError::Storage)?;
    if revision != expected_value {
        return Err(ImportError::StalePlan);
    }

    if let Some(member_value) = member_id {
        let exists: Option<i64> = transaction
            .query_row(
                "SELECT 1 FROM members WHERE id = ?1 AND profile_id = ?2",
                params![member_value, profile_value],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage_error)?;
        if exists.is_none() {
            return Err(ImportError::MemberNotFound);
        }
    }

    transaction
        .execute(
            "INSERT INTO imports \
             (profile_id, expected_revision, target, display_name, member_id, state) \
             VALUES (?1, ?2, ?3, ?4, ?5, 'writing')",
            params![profile_value, expected_value, kind, display_name, member_id],
        )
        .map_err(storage_error)?;

    let rowid = transaction.last_insert_rowid();
    let id = ImportId::try_from_u64(u64::try_from(rowid).map_err(|_| ImportError::Storage)?)?;
    transaction.commit().map_err(storage_error)?;
    Ok(id)
}

#[cfg(test)]
fn copy_archive(input: &mut dyn Read, path: &Path) -> Result<[u8; 32], ImportError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| ImportError::Storage)?;
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];

    loop {
        let read = input
            .read(&mut buffer)
            .map_err(|_| ImportError::Archive(ArchiveError::Io))?;
        if read == 0 {
            break;
        }
        total = total.checked_add(read as u64).ok_or(ImportError::Archive(
            ArchiveError::LimitExceeded {
                kind: marduk_core::import::LimitKind::ArchiveBytes,
            },
        ))?;
        if total > M1_ZIP_LIMITS.max_archive_bytes {
            return Err(ImportError::Archive(ArchiveError::LimitExceeded {
                kind: marduk_core::import::LimitKind::ArchiveBytes,
            }));
        }
        hasher.update(&buffer[..read]);
        file.write_all(&buffer[..read])
            .map_err(|_| ImportError::Storage)?;
    }

    file.sync_all().map_err(|_| ImportError::Storage)?;
    Ok(hasher.finalize().into())
}

#[cfg(test)]
struct HashingWriter {
    file: File,
    hasher: Sha256,
}

#[cfg(test)]
impl HashingWriter {
    fn new(file: File) -> Self {
        Self {
            file,
            hasher: Sha256::new(),
        }
    }

    fn finish(self) -> ([u8; 32], File) {
        (self.hasher.finalize().into(), self.file)
    }
}

#[cfg(test)]
impl std::io::Write for HashingWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let written = self.file.write(buffer)?;
        self.hasher.update(&buffer[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

fn persist_stage(
    connection: &mut Connection,
    id: ImportId,
    archive_sha256: &[u8; 32],
    owner: &[u8],
    files: &[PayloadFile],
) -> Result<(), ImportError> {
    let import_value = sql_import_id(id)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage_error)?;

    for (ordinal, file) in files.iter().enumerate() {
        let ordinal = i64::try_from(ordinal).map_err(|_| ImportError::Storage)?;
        let len = i64::try_from(file.len).map_err(|_| ImportError::Storage)?;
        transaction
            .execute(
                "INSERT INTO staged_files (import_id, ordinal, role, path, len, sha) \
                 VALUES (?1, ?2, 'data', ?3, ?4, ?5)",
                params![
                    import_value,
                    ordinal,
                    file.target.relative.as_str(),
                    len,
                    file.sha256.as_slice()
                ],
            )
            .map_err(storage_error)?;
    }

    let changed = transaction
        .execute(
            "UPDATE imports SET state = 'prepared', archive_sha = ?1, owner = ?2 \
             WHERE id = ?3 AND state = 'writing'",
            params![archive_sha256.as_slice(), owner, import_value],
        )
        .map_err(storage_error)?;
    if changed != 1 {
        return Err(ImportError::Storage);
    }
    transaction.commit().map_err(storage_error)
}

fn read_snapshot(
    transaction: &Transaction<'_>,
    profile: ProfileId,
) -> Result<ProfileSnapshot, ImportError> {
    let profile_value = sql_profile_id(profile)?;
    let revision: Option<i64> = transaction
        .query_row(
            "SELECT revision FROM profiles WHERE id = ?1",
            [profile_value],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage_error)?;
    let revision = revision.ok_or(ImportError::Storage)?;

    let rows = {
        let mut statement = transaction
            .prepare(
                "SELECT id, installed_id, enabled FROM members \
                 WHERE profile_id = ?1 ORDER BY ordinal ASC",
            )
            .map_err(storage_error)?;
        let rows = statement
            .query_map([profile_value], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(storage_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(storage_error)?
    };

    let mut members = Vec::with_capacity(rows.len());
    for (id, installed, enabled) in rows {
        members.push(ProfileMember {
            id: MemberId::new(positive_id(id)?),
            installed: InstalledRevisionId::new(positive_id(installed)?),
            enabled: enabled_flag(enabled)?,
        });
    }

    Ok(ProfileSnapshot {
        id: profile,
        revision: ProfileRevision::new(nonnegative_value(revision)?),
        members,
    })
}

fn read_names(
    transaction: &Transaction<'_>,
    profile: ProfileId,
) -> Result<BTreeMap<MemberId, String>, ImportError> {
    let profile_value = sql_profile_id(profile)?;
    let rows = {
        let mut statement = transaction
            .prepare("SELECT id, display_name FROM members WHERE profile_id = ?1")
            .map_err(storage_error)?;
        let rows = statement
            .query_map([profile_value], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(storage_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(storage_error)?
    };

    let mut names = BTreeMap::new();
    for (id, name) in rows {
        names.insert(MemberId::new(positive_id(id)?), name);
    }
    Ok(names)
}

pub(crate) fn read_profile_revision(
    connection: &Connection,
    profile: ProfileId,
) -> Result<ProfileRevision, ImportError> {
    let profile_value = sql_profile_id(profile)?;
    let revision: Option<i64> = connection
        .query_row(
            "SELECT revision FROM profiles WHERE id = ?1",
            [profile_value],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage_error)?;
    let revision = revision.ok_or(ImportError::Storage)?;
    Ok(ProfileRevision::new(nonnegative_value(revision)?))
}

fn ensure_member(
    connection: &Connection,
    profile: ProfileId,
    member: MemberId,
) -> Result<(), ImportError> {
    let profile_value = sql_profile_id(profile)?;
    let member_value = sql_member_id(member)?;
    let exists: Option<i64> = connection
        .query_row(
            "SELECT 1 FROM members WHERE id = ?1 AND profile_id = ?2",
            params![member_value, profile_value],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage_error)?;
    if exists.is_none() {
        return Err(ImportError::MemberNotFound);
    }
    Ok(())
}

fn next_ordinal(transaction: &Transaction<'_>, profile: i64) -> Result<i64, ImportError> {
    let maximum: Option<i64> = transaction
        .query_row(
            "SELECT MAX(ordinal) FROM members WHERE profile_id = ?1",
            [profile],
            |row| row.get(0),
        )
        .map_err(storage_error)?;
    let ordinal = match maximum {
        Some(value) => value.checked_add(1).ok_or(ImportError::Storage)?,
        None => 0,
    };
    if ordinal < 0 {
        return Err(ImportError::Storage);
    }
    Ok(ordinal)
}

fn read_result_snapshot(
    connection: &Connection,
    id: ImportId,
) -> Result<ProfileSnapshot, ImportError> {
    let import_value = sql_import_id(id)?;
    let row: Option<(i64, Option<i64>)> = connection
        .query_row(
            "SELECT profile_id, result_revision FROM imports WHERE id = ?1",
            [import_value],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(storage_error)?;
    let (profile_value, result_revision) = row.ok_or(ImportError::UnknownImport)?;
    let result_revision = result_revision.ok_or(ImportError::Storage)?;

    let rows = {
        let mut statement = connection
            .prepare(
                "SELECT member_id, installed_id, enabled FROM import_result_members \
                 WHERE import_id = ?1 ORDER BY ordinal ASC",
            )
            .map_err(storage_error)?;
        let rows = statement
            .query_map([import_value], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(storage_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(storage_error)?
    };

    let mut members = Vec::with_capacity(rows.len());
    for (member, installed, enabled) in rows {
        members.push(ProfileMember {
            id: MemberId::new(positive_id(member)?),
            installed: InstalledRevisionId::new(positive_id(installed)?),
            enabled: enabled_flag(enabled)?,
        });
    }

    Ok(ProfileSnapshot {
        id: ProfileId::new(positive_id(profile_value)?),
        revision: ProfileRevision::new(nonnegative_value(result_revision)?),
        members,
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ImportState {
    Writing,
    Prepared,
    Committed,
    Discarded,
}

impl ImportState {
    fn parse(value: &str) -> Result<Self, ImportError> {
        match value {
            "writing" => Ok(Self::Writing),
            "prepared" => Ok(Self::Prepared),
            "committed" => Ok(Self::Committed),
            "discarded" => Ok(Self::Discarded),
            _ => Err(ImportError::Storage),
        }
    }
}

struct ImportRecord {
    id: ImportId,
    profile: ProfileId,
    expected_revision: ProfileRevision,
    target: ImportTarget,
    state: ImportState,
    owner: Option<Vec<u8>>,
    archive_sha256: Option<[u8; 32]>,
}

fn load_import(connection: &Connection, id: ImportId) -> Result<ImportRecord, ImportError> {
    let import_value = sql_import_id(id)?;
    let row = connection
        .query_row(
            "SELECT profile_id, expected_revision, target, display_name, member_id, \
             state, owner, archive_sha FROM imports WHERE id = ?1",
            [import_value],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<Vec<u8>>>(6)?,
                    row.get::<_, Option<Vec<u8>>>(7)?,
                ))
            },
        )
        .optional()
        .map_err(storage_error)?
        .ok_or(ImportError::UnknownImport)?;

    let (profile, expected_revision, target, display_name, member_id, state, owner, archive_sha) =
        row;
    let target = match target.as_str() {
        "append" => ImportTarget::Append {
            display_name: display_name.ok_or(ImportError::Storage)?,
        },
        "replace" => ImportTarget::Replace {
            member: MemberId::new(positive_id(member_id.ok_or(ImportError::Storage)?)?),
        },
        _ => return Err(ImportError::Storage),
    };
    let archive_sha256 = match archive_sha {
        Some(bytes) => Some(sha_from_blob(bytes)?),
        None => None,
    };

    Ok(ImportRecord {
        id,
        profile: ProfileId::new(positive_id(profile)?),
        expected_revision: ProfileRevision::new(nonnegative_value(expected_revision)?),
        target,
        state: ImportState::parse(&state)?,
        owner,
        archive_sha256,
    })
}

#[cfg_attr(not(test), allow(dead_code))]
struct StoredFile {
    path: RelativePath,
    len: u64,
    sha: [u8; 32],
}

fn stored_files(connection: &Connection, id: ImportId) -> Result<Vec<StoredFile>, ImportError> {
    let import_value = sql_import_id(id)?;
    let rows = {
        let mut statement = connection
            .prepare(
                "SELECT path, len, sha FROM staged_files \
                 WHERE import_id = ?1 ORDER BY ordinal ASC",
            )
            .map_err(storage_error)?;
        let rows = statement
            .query_map([import_value], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                ))
            })
            .map_err(storage_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(storage_error)?
    };

    let mut files = Vec::with_capacity(rows.len());
    for (path, len, sha) in rows {
        files.push(StoredFile {
            path: RelativePath::parse(&path).map_err(|_| ImportError::Storage)?,
            len: nonnegative_value(len)?,
            sha: sha_from_blob(sha)?,
        });
    }
    Ok(files)
}

#[cfg(test)]
struct ExpectedFile {
    path: String,
    len: u64,
    sha: [u8; 32],
}

#[cfg(test)]
fn verify_files(
    directory: &Path,
    archive_sha256: &[u8; 32],
    files: &[ExpectedFile],
) -> Result<(), ImportError> {
    let archive_path = directory.join(ARCHIVE_FILE);
    let (actual, _) = hash_file(&archive_path).map_err(|_| ImportError::StageChanged)?;
    if actual != *archive_sha256 {
        return Err(ImportError::StageChanged);
    }

    // The private snapshot is still a bounded, admissible container, not just
    // bytes with a matching digest.
    let archive = File::open(&archive_path).map_err(|_| ImportError::StageChanged)?;
    admit_zip(archive, M1_ZIP_LIMITS).map_err(|_| ImportError::StageChanged)?;

    let payload_root = directory.join(PAYLOAD_DIR);
    let entries = fs::read_dir(directory).map_err(|_| ImportError::StageChanged)?;
    for entry in entries {
        let entry = entry.map_err(|_| ImportError::StageChanged)?;
        let name = entry.file_name();
        let kind = entry.file_type().map_err(|_| ImportError::StageChanged)?;
        match name.to_str() {
            Some(ARCHIVE_FILE) if kind.is_file() => {}
            Some(PAYLOAD_DIR) if kind.is_dir() => {}
            _ => return Err(ImportError::StageChanged),
        }
    }

    let mut found = BTreeSet::new();
    if payload_root.is_dir() {
        collect_payload_files(&payload_root, "", &mut found)?;
    } else if !files.is_empty() {
        return Err(ImportError::StageChanged);
    }

    let mut expected = BTreeSet::new();
    for file in files {
        if !expected.insert(file.path.clone()) {
            return Err(ImportError::StageChanged);
        }
        let path = payload_file(directory, &file.path);
        let (sha, len) = hash_file(&path).map_err(|_| ImportError::StageChanged)?;
        if sha != file.sha || len != file.len {
            return Err(ImportError::StageChanged);
        }
    }

    if found != expected {
        return Err(ImportError::StageChanged);
    }
    Ok(())
}

#[cfg(test)]
fn collect_payload_files(
    root: &Path,
    prefix: &str,
    found: &mut BTreeSet<String>,
) -> Result<(), ImportError> {
    let entries = fs::read_dir(root).map_err(|_| ImportError::StageChanged)?;
    for entry in entries {
        let entry = entry.map_err(|_| ImportError::StageChanged)?;
        let name = entry
            .file_name()
            .to_str()
            .map(str::to_owned)
            .ok_or(ImportError::StageChanged)?;
        let relative = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let kind = entry.file_type().map_err(|_| ImportError::StageChanged)?;
        if kind.is_dir() {
            collect_payload_files(&entry.path(), &relative, found)?;
        } else if kind.is_file() {
            found.insert(relative);
        } else {
            return Err(ImportError::StageChanged);
        }
    }
    Ok(())
}

#[cfg(test)]
fn hash_file(path: &Path) -> std::io::Result<([u8; 32], u64)> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(read as u64)
            .ok_or_else(|| std::io::Error::other("file too large"))?;
        hasher.update(&buffer[..read]);
    }
    Ok((hasher.finalize().into(), total))
}

fn is_published(connection: &Connection, id: ImportId) -> Result<bool, ImportError> {
    let import_value = sql_import_id(id)?;
    let found: Option<i64> = connection
        .query_row(
            "SELECT 1 FROM installed_revisions WHERE id = ?1",
            [import_value],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage_error)?;
    Ok(found.is_some())
}

fn import_ids_in_state(connection: &Connection, state: &str) -> Result<Vec<ImportId>, ImportError> {
    let rows = {
        let mut statement = connection
            .prepare("SELECT id FROM imports WHERE state = ?1 ORDER BY id")
            .map_err(storage_error)?;
        let rows = statement
            .query_map([state], |row| row.get::<_, i64>(0))
            .map_err(storage_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(storage_error)?
    };

    let mut ids = Vec::with_capacity(rows.len());
    for value in rows {
        let value = u64::try_from(value).map_err(|_| ImportError::Storage)?;
        ids.push(ImportId::try_from_u64(value)?);
    }
    Ok(ids)
}

#[cfg(test)]
fn fixture_receipt(stage: &StageRecord) -> Result<(&[u8; 32], &[u8]), ImportError> {
    match (stage.archive_sha256.as_ref(), stage.owner.as_deref()) {
        (Some(archive_sha256), Some(owner)) => Ok((archive_sha256, owner)),
        _ => Err(ImportError::StageChanged),
    }
}

#[cfg(test)]
fn expected_files(files: &[StoredFile]) -> Vec<ExpectedFile> {
    files
        .iter()
        .map(|file| ExpectedFile {
            path: file.path.as_str().to_owned(),
            len: file.len,
            sha: file.sha,
        })
        .collect()
}

#[cfg(test)]
fn remove_file_if_present(path: &Path) -> Result<(), ImportError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(ImportError::Storage),
    }
}

#[cfg(test)]
fn prune_known_directories(directory: &Path, files: &[StoredFile]) -> Result<(), ImportError> {
    // Allowed directories are exactly the payload root and the parents of
    // staged files. The archive's parent is the revision root itself.
    let mut allowed = BTreeSet::new();
    allowed.insert(PAYLOAD_DIR.to_owned());
    for file in files {
        let components = file.path.as_str().split('/').collect::<Vec<_>>();
        let mut prefix = String::new();
        for component in &components[..components.len().saturating_sub(1)] {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(component);
            allowed.insert(format!("{PAYLOAD_DIR}/{prefix}"));
        }
    }

    prune_children(directory, "", &allowed)?;
    match fs::remove_dir(directory) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {
            Err(ImportError::StageChanged)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(ImportError::Storage),
    }
}

#[cfg(test)]
fn prune_children(
    directory: &Path,
    relative: &str,
    allowed: &BTreeSet<String>,
) -> Result<(), ImportError> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(ImportError::Storage),
    };

    for entry in entries {
        let entry = entry.map_err(|_| ImportError::Storage)?;
        let name = entry
            .file_name()
            .to_str()
            .map(str::to_owned)
            .ok_or(ImportError::StageChanged)?;
        let child = if relative.is_empty() {
            name
        } else {
            format!("{relative}/{name}")
        };
        let kind = entry.file_type().map_err(|_| ImportError::Storage)?;

        if kind.is_dir() {
            if !allowed.contains(&child) {
                // Unexpected directory: refuse and leave it untouched.
                return Err(ImportError::StageChanged);
            }
            prune_children(&entry.path(), &child, allowed)?;
            match fs::remove_dir(entry.path()) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {
                    return Err(ImportError::StageChanged);
                }
                Err(_) => return Err(ImportError::Storage),
            }
        } else {
            // Known files were removed before pruning; a remaining entry is
            // unexpected and must be preserved.
            return Err(ImportError::StageChanged);
        }
    }
    Ok(())
}

#[cfg(test)]
fn sync_parent_chain(path: &Path, root: &Path) -> Result<(), ImportError> {
    let mut current = path.parent();
    while let Some(directory) = current {
        sync_directory(directory)?;
        if directory == root {
            break;
        }
        current = directory.parent();
    }
    Ok(())
}

#[cfg(all(test, unix))]
fn sync_directory(directory: &Path) -> Result<(), ImportError> {
    File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(|_| ImportError::Storage)
}

#[cfg(all(test, not(unix)))]
fn sync_directory(_directory: &Path) -> Result<(), ImportError> {
    // Windows does not expose directory handles through std; best-effort only.
    // Native namespace durability is a Task 5 qualification, not claimed here.
    Ok(())
}

fn validate_permutation(members: &[MemberRow], order: &[MemberId]) -> Result<(), ProfileEditError> {
    if order.len() != members.len() {
        return Err(ProfileEditError::InvalidOrder);
    }
    let known = members
        .iter()
        .map(|member| member.id.get())
        .collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();
    for member in order {
        let value = member.get();
        if value == 0 || !known.contains(&value) || !seen.insert(value) {
            return Err(ProfileEditError::InvalidOrder);
        }
    }
    if seen.len() != members.len() {
        return Err(ProfileEditError::InvalidOrder);
    }
    Ok(())
}

struct MemberRow {
    id: MemberId,
    installed: InstalledRevisionId,
    enabled: bool,
    ordinal: i64,
}

fn read_member_rows(
    transaction: &Transaction<'_>,
    profile: i64,
) -> Result<Vec<MemberRow>, ImportError> {
    let rows = {
        let mut statement = transaction
            .prepare(
                "SELECT id, installed_id, enabled, ordinal FROM members \
                 WHERE profile_id = ?1 ORDER BY ordinal ASC",
            )
            .map_err(storage_error)?;
        let rows = statement
            .query_map([profile], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .map_err(storage_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(storage_error)?
    };

    let mut members = Vec::with_capacity(rows.len());
    for (id, installed, enabled, ordinal) in rows {
        if ordinal < 0 {
            return Err(ImportError::Storage);
        }
        members.push(MemberRow {
            id: MemberId::new(positive_id(id)?),
            installed: InstalledRevisionId::new(positive_id(installed)?),
            enabled: enabled_flag(enabled)?,
            ordinal,
        });
    }
    Ok(members)
}

#[cfg(test)]
fn revision_dir(root: &Path, id: ImportId) -> PathBuf {
    root.join(REVISIONS_DIR).join(id.get().to_string())
}

#[cfg(test)]
fn payload_file(revision_directory: &Path, relative: &str) -> PathBuf {
    let mut path = revision_directory.join(PAYLOAD_DIR);
    for component in relative.split('/') {
        path.push(component);
    }
    path
}

#[cfg(test)]
fn synthetic_owner(id: ImportId, archive_sha256: &[u8; 32]) -> RootIdentity {
    let mut fs_id = [0u8; 16];
    fs_id.copy_from_slice(&archive_sha256[..16]);
    let mut birth_bytes = [0u8; 8];
    birth_bytes.copy_from_slice(&archive_sha256[16..24]);
    let birth = i64::from_le_bytes(birth_bytes);

    #[cfg(windows)]
    {
        RootIdentity::from_adapter_evidence(NativeIdentity::Windows {
            volume_serial: id.get(),
            file_id: fs_id,
            creation_100ns: birth,
        })
    }
    #[cfg(not(windows))]
    {
        RootIdentity::from_adapter_evidence(NativeIdentity::Linux {
            fs_magic: 0x4d31_0001, // synthetic fixture marker, not a native filesystem
            fs_id,
            view_id: id.get(),
            tree_id: Some(id.get()),
            ino: id.get(),
            birth: (birth, 0),
        })
    }
}

fn native_evidence(stage: &StageRecord) -> Result<StageEvidence, ImportError> {
    let archive_sha256 = stage.archive_sha256.ok_or(ImportError::StageChanged)?;
    let owner = crate::operation_journal::decode_root(
        stage.owner.as_deref().ok_or(ImportError::StageChanged)?,
    )
    .map_err(|_| ImportError::StageChanged)?;
    let files = stage
        .files
        .iter()
        .map(|file| PayloadFile {
            target: TargetFile {
                root: RootRole::Data,
                relative: file.path.clone(),
            },
            len: file.len,
            sha256: file.sha,
        })
        .collect();
    Ok(StageEvidence {
        allocation: ImportAllocation {
            id: stage.id,
            revision: InstalledRevisionId::new(stage.id.get()),
        },
        archive_sha256,
        files,
        owner,
    })
}

fn sha_from_blob(blob: Vec<u8>) -> Result<[u8; 32], ImportError> {
    blob.try_into().map_err(|_| ImportError::Storage)
}

fn positive_id(value: i64) -> Result<u64, ImportError> {
    match u64::try_from(value) {
        Ok(value) if value > 0 => Ok(value),
        _ => Err(ImportError::Storage),
    }
}

fn nonnegative_value(value: i64) -> Result<u64, ImportError> {
    u64::try_from(value).map_err(|_| ImportError::Storage)
}

fn enabled_flag(value: i64) -> Result<bool, ImportError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(ImportError::Storage),
    }
}

fn sql_profile_id(profile: ProfileId) -> Result<i64, ImportError> {
    if profile.get() == 0 {
        return Err(ImportError::Storage);
    }
    nonnegative_sql(profile.get())
}

fn sql_member_id(member: MemberId) -> Result<i64, ImportError> {
    if member.get() == 0 {
        return Err(ImportError::Storage);
    }
    nonnegative_sql(member.get())
}

fn sql_import_id(id: ImportId) -> Result<i64, ImportError> {
    nonnegative_sql(id.get())
}

fn nonnegative_sql(value: u64) -> Result<i64, ImportError> {
    i64::try_from(value).map_err(|_| ImportError::Storage)
}

fn gate_error(error: GateError) -> ImportError {
    match error {
        GateError::Busy => ImportError::Busy,
        GateError::Unavailable | GateError::Io => ImportError::Storage,
    }
}

fn edit_storage(_error: rusqlite::Error) -> ProfileEditError {
    ProfileEditError::Storage
}

fn storage_error(_error: rusqlite::Error) -> ImportError {
    ImportError::Storage
}
