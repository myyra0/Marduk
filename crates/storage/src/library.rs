//! SQLite-backed immutable library catalog.
//!
//! Only `open`, `named_profile` and `snapshot` are implemented in the scaffold.
//! Prepared imports, publication, discard, reconciliation and reordering are
//! frozen as typed refusals until the stage adapter exists.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use marduk_core::ids::{InstalledRevisionId, MemberId, ProfileId, ProfileRevision};
use marduk_core::import::{
    BeginImport, ImportError, ImportId, NamedProfile, PreparedImport, ProfileEditError,
};
use marduk_core::profiles::{ProfileMember, ProfileSnapshot};
use marduk_platform::{GateError, WriterGate};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior};

#[cfg(test)]
mod contract_tests;

const CATALOG_FILE: &str = "catalog.sqlite3";
const LOCK_FILE: &str = "library.lock";
const REVISIONS_DIR: &str = "revisions";
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const MIGRATION_VERSION: i64 = 1;
const SCHEMA_V1: &str = include_str!("../migrations/0001_library.sql");

pub struct Catalog {
    root: PathBuf,
    connection: Connection,
    stage: StageAdapter,
}

impl Catalog {
    /// Open the local catalog, creating its root and `revisions/` directory.
    ///
    /// Configuration and migration run under the library writer gate. PRAGMAs
    /// are read back from the live connection and a mismatch is an error.
    pub fn open(root: &Path) -> Result<Self, ImportError> {
        fs::create_dir_all(root).map_err(|_| ImportError::Storage)?;
        fs::create_dir_all(root.join(REVISIONS_DIR)).map_err(|_| ImportError::Storage)?;

        let gate = WriterGate::open(root.join(LOCK_FILE)).map_err(gate_error)?;
        let _guard = gate.try_lock().map_err(gate_error)?;

        let mut connection = Connection::open(root.join(CATALOG_FILE)).map_err(storage_error)?;
        configure(&connection)?;
        migrate(&mut connection)?;
        seed_profile(&mut connection)?;
        verify_pragmas(&connection)?;

        Ok(Self {
            root: root.to_path_buf(),
            connection,
            stage: StageAdapter::Unavailable(UnavailableStage),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
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

    pub fn prepare_import(
        &mut self,
        mut input: impl Read,
        plan: BeginImport,
    ) -> Result<PreparedImport, ImportError> {
        self.stage.prepare(&mut input, plan)
    }

    pub fn publish_import(&mut self, _id: ImportId) -> Result<ProfileSnapshot, ImportError> {
        Err(ImportError::Storage)
    }

    pub fn discard_import(&mut self, _id: ImportId) -> Result<(), ImportError> {
        Err(ImportError::Storage)
    }

    pub fn reconcile(&mut self) -> Result<Vec<ImportId>, ImportError> {
        Err(ImportError::Storage)
    }

    pub fn reorder(
        &mut self,
        _profile: ProfileId,
        _expected: ProfileRevision,
        _order: &[MemberId],
    ) -> Result<ProfileSnapshot, ProfileEditError> {
        Err(ProfileEditError::Storage)
    }

    /// Test-only constructor used by the frozen library contract oracle.
    #[cfg(test)]
    pub(crate) fn open_for_test(root: &Path) -> Result<Self, ImportError> {
        let mut catalog = Self::open(root)?;
        catalog.stage = StageAdapter::Fixture(FixtureStage);
        Ok(catalog)
    }
}

/// Private stage seam. There is no public constructor or product feature that
/// can substitute another adapter.
enum StageAdapter {
    Unavailable(UnavailableStage),
    #[cfg(test)]
    Fixture(FixtureStage),
}

impl StageAdapter {
    fn prepare(
        &mut self,
        input: &mut dyn Read,
        plan: BeginImport,
    ) -> Result<PreparedImport, ImportError> {
        match self {
            Self::Unavailable(stage) => stage.prepare(input, plan),
            #[cfg(test)]
            Self::Fixture(stage) => stage.prepare(input, plan),
        }
    }
}

/// Production seam. Native payload publication arrives in Task 5; until a
/// qualified adapter exists every prepare attempt is refused rather than faked.
struct UnavailableStage;

impl UnavailableStage {
    fn prepare(
        &mut self,
        _input: &mut dyn Read,
        _plan: BeginImport,
    ) -> Result<PreparedImport, ImportError> {
        Err(ImportError::StageUnavailable)
    }
}

/// Test-only fixture seam. The contract oracle replaces `contract_tests.rs` and
/// this adapter is filled in with the implementation.
#[cfg(test)]
struct FixtureStage;

#[cfg(test)]
impl FixtureStage {
    fn prepare(
        &mut self,
        _input: &mut dyn Read,
        _plan: BeginImport,
    ) -> Result<PreparedImport, ImportError> {
        Err(ImportError::StageUnavailable)
    }
}

fn configure(connection: &Connection) -> Result<(), ImportError> {
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(storage_error)?;
    connection
        .pragma_update(None, "foreign_keys", 1)
        .map_err(storage_error)?;
    connection
        .pragma_update(None, "synchronous", 2)
        .map_err(storage_error)?;

    // `journal_mode` cannot change inside a transaction; the returned value is
    // the mode SQLite actually selected.
    let _: String = connection
        .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))
        .map_err(storage_error)?;
    Ok(())
}

fn migrate(connection: &mut Connection) -> Result<(), ImportError> {
    let version = user_version(connection)?;
    if version < 0 || version > MIGRATION_VERSION {
        return Err(ImportError::Storage);
    }

    if version < MIGRATION_VERSION {
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        if user_version(&transaction)? != version {
            return Err(ImportError::Storage);
        }
        transaction
            .execute_batch(SCHEMA_V1)
            .map_err(storage_error)?;
        transaction
            .pragma_update(None, "user_version", MIGRATION_VERSION)
            .map_err(storage_error)?;
        transaction.commit().map_err(storage_error)?;
    }

    if user_version(connection)? != MIGRATION_VERSION {
        return Err(ImportError::Storage);
    }
    Ok(())
}

fn seed_profile(connection: &mut Connection) -> Result<(), ImportError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage_error)?;
    transaction
        .execute(
            "INSERT INTO profiles (id, revision) VALUES (1, 0) ON CONFLICT (id) DO NOTHING",
            [],
        )
        .map_err(storage_error)?;
    transaction.commit().map_err(storage_error)
}

fn verify_pragmas(connection: &Connection) -> Result<(), ImportError> {
    let foreign_keys: i64 = connection
        .pragma_query_value(None, "foreign_keys", |row| row.get(0))
        .map_err(storage_error)?;
    if foreign_keys != 1 {
        return Err(ImportError::Storage);
    }

    let journal_mode: String = connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .map_err(storage_error)?;
    if !journal_mode.eq_ignore_ascii_case("wal") {
        return Err(ImportError::Storage);
    }

    let synchronous: i64 = connection
        .pragma_query_value(None, "synchronous", |row| row.get(0))
        .map_err(storage_error)?;
    if synchronous != 2 {
        return Err(ImportError::Storage);
    }

    Ok(())
}

fn user_version(connection: &Connection) -> Result<i64, ImportError> {
    connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(storage_error)
}

fn read_snapshot(
    transaction: &Transaction<'_>,
    profile: ProfileId,
) -> Result<ProfileSnapshot, ImportError> {
    let profile_value = i64::try_from(profile.get()).map_err(|_| ImportError::Storage)?;
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
    let profile_value = i64::try_from(profile.get()).map_err(|_| ImportError::Storage)?;
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

fn gate_error(error: GateError) -> ImportError {
    match error {
        GateError::Busy => ImportError::Busy,
        GateError::Unavailable | GateError::Io => ImportError::Storage,
    }
}

fn storage_error(_error: rusqlite::Error) -> ImportError {
    ImportError::Storage
}
