//! Single owner of the catalog file, connection PRAGMAs, schema migrations and
//! seed rows. Both the library catalog and the operation journal open the same
//! `catalog.sqlite3` through this module.

use std::time::Duration;

use rusqlite::{Connection, TransactionBehavior};

pub(crate) const CATALOG_FILE: &str = "catalog.sqlite3";
pub(crate) const LOCK_FILE: &str = "library.lock";
pub(crate) const JOURNAL_LOCK_FILE: &str = "journal.lock";
pub(crate) const MIGRATION_VERSION: i64 = 2;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const SCHEMA_V1: &str = include_str!("../migrations/0001_library.sql");
const SCHEMA_V2: &str = include_str!("../migrations/0002_operations.sql");
const RECOVERY_PROOFS: &str = include_str!("../migrations/v2_recovery_proofs.sql");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SchemaError;

impl From<rusqlite::Error> for SchemaError {
    fn from(_: rusqlite::Error) -> Self {
        Self
    }
}

/// Configure, migrate to the current schema, seed the profile row and read the
/// PRAGMAs back from the live connection.
pub(crate) fn prepare(connection: &mut Connection) -> Result<(), SchemaError> {
    configure(connection)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    migrate(&transaction)?;
    transaction.execute_batch(RECOVERY_PROOFS)?;
    transaction.execute(
        "INSERT INTO profiles (id, revision) VALUES (1, 0) ON CONFLICT (id) DO NOTHING",
        [],
    )?;
    transaction.commit()?;
    verify_pragmas(connection)
}

/// Reconfigure an existing connection and re-verify the required PRAGMAs
/// without running migrations or writing seed rows.
pub(crate) fn reconfigure(connection: &Connection) -> Result<(), SchemaError> {
    configure(connection)?;
    verify_pragmas(connection)
}

fn configure(connection: &Connection) -> Result<(), SchemaError> {
    connection.busy_timeout(BUSY_TIMEOUT)?;
    connection.pragma_update(None, "foreign_keys", 1)?;
    connection.pragma_update(None, "synchronous", 2)?;
    // `journal_mode` cannot change inside a transaction; the returned value is
    // the mode SQLite actually selected.
    let _: String =
        connection.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
    Ok(())
}

fn migrate(connection: &Connection) -> Result<(), SchemaError> {
    let version = user_version(connection)?;
    if version < 0 || version > MIGRATION_VERSION {
        return Err(SchemaError);
    }

    if version < 1 {
        connection.execute_batch(SCHEMA_V1)?;
    }

    if version < 2 {
        connection.execute_batch(SCHEMA_V2)?;
        connection.pragma_update(None, "user_version", 2)?;
    }

    if user_version(connection)? != MIGRATION_VERSION {
        return Err(SchemaError);
    }
    Ok(())
}

fn verify_pragmas(connection: &Connection) -> Result<(), SchemaError> {
    let foreign_keys: i64 =
        connection.pragma_query_value(None, "foreign_keys", |row| row.get(0))?;
    let journal_mode: String =
        connection.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
    let synchronous: i64 = connection.pragma_query_value(None, "synchronous", |row| row.get(0))?;
    if foreign_keys != 1 || !journal_mode.eq_ignore_ascii_case("wal") || synchronous != 2 {
        return Err(SchemaError);
    }
    Ok(())
}

fn user_version(connection: &Connection) -> Result<i64, SchemaError> {
    connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(Into::into)
}
