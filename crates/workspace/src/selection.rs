//! One-use selections for setup and import.
//!
//! The CLI names a path directly; the desktop shell only ever sees the opaque
//! [`SelectionId`] minted here. The owning module resolves the handle, so a
//! renderer can never supply a path, replay a handle or reuse one across
//! operations. Archive picks are opened immediately and consumed as a file
//! descriptor; installation picks are canonicalized once, before validation.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::path::PathBuf;

use crate::{Code, WireError};

/// Why a native selection was recorded. A handle from one picker is never
/// accepted by the other operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    Installation,
    Archive,
}

/// Trusted process-local handle. Only its decimal form crosses the IPC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SelectionId(u64);

impl SelectionId {
    pub fn parse(value: &str) -> Result<Self, WireError> {
        let digits = !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit());
        let canonical = value.len() == 1 || !value.starts_with('0');
        if !digits || !canonical {
            return Err(WireError::new(Code::InvalidRequest));
        }
        value
            .parse()
            .ok()
            .filter(|id| *id > 0 && *id <= i64::MAX as u64)
            .map(Self)
            .ok_or_else(|| WireError::new(Code::InvalidRequest))
    }
}

impl fmt::Display for SelectionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Where a setup or import input came from. The wire never carries this type;
/// each client decoder builds it from its own frozen argument shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selection {
    CliPath(PathBuf),
    Desktop(SelectionId),
}

#[derive(Debug)]
pub(crate) enum Source {
    Installation(PathBuf),
    Archive(File),
}

/// Bounds process memory if a client keeps opening pickers without consuming.
const MAX_SELECTIONS: usize = 32;

pub(crate) struct SelectionStore {
    next: u64,
    entries: BTreeMap<u64, Entry>,
}

struct Entry {
    purpose: Purpose,
    source: Source,
}

impl SelectionStore {
    pub(crate) fn new() -> Self {
        Self {
            next: 1,
            entries: BTreeMap::new(),
        }
    }

    pub(crate) fn insert(
        &mut self,
        purpose: Purpose,
        path: PathBuf,
    ) -> Result<SelectionId, WireError> {
        let source = match purpose {
            Purpose::Archive => {
                Source::Archive(File::open(&path).map_err(|_| WireError::new(Code::Io))?)
            }
            Purpose::Installation => {
                let resolved =
                    std::fs::canonicalize(&path).map_err(|_| WireError::new(Code::InvalidPath))?;
                Source::Installation(resolved)
            }
        };
        if self.entries.len() >= MAX_SELECTIONS
            && let Some(oldest) = self.entries.keys().next().copied()
        {
            self.entries.remove(&oldest);
        }
        let id = self.next;
        self.next += 1;
        self.entries.insert(id, Entry { purpose, source });
        Ok(SelectionId(id))
    }

    pub(crate) fn take(
        &mut self,
        selection_id: SelectionId,
        purpose: Purpose,
    ) -> Result<Source, WireError> {
        let entry = self
            .entries
            .remove(&selection_id.0)
            .ok_or_else(|| WireError::new(Code::InvalidRequest))?;
        if entry.purpose != purpose {
            return Err(WireError::new(Code::InvalidRequest));
        }
        Ok(entry.source)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn handles_are_single_use() {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("game");
        fs::create_dir(&game).unwrap();
        let mut store = SelectionStore::new();

        let id = store.insert(Purpose::Installation, game.clone()).unwrap();
        match store.take(id, Purpose::Installation).unwrap() {
            Source::Installation(path) => assert_eq!(path, fs::canonicalize(&game).unwrap()),
            Source::Archive(_) => panic!("wrong source kind"),
        }
        assert_eq!(
            store.take(id, Purpose::Installation).unwrap_err(),
            WireError::new(Code::InvalidRequest)
        );
    }

    #[test]
    fn archive_selections_are_opened_before_the_handle_is_minted() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("mods.zip");
        fs::write(&archive, b"not a zip").unwrap();
        let mut store = SelectionStore::new();

        let id = store.insert(Purpose::Archive, archive).unwrap();
        assert!(matches!(
            store.take(id, Purpose::Archive).unwrap(),
            Source::Archive(_)
        ));
    }

    #[test]
    fn a_handle_is_bound_to_its_purpose() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("mods.zip");
        fs::write(&archive, b"not a zip").unwrap();
        let mut store = SelectionStore::new();

        let id = store.insert(Purpose::Archive, archive).unwrap();
        assert_eq!(
            store.take(id, Purpose::Installation).unwrap_err(),
            WireError::new(Code::InvalidRequest)
        );
        // Consumed even by the refused attempt, so it cannot be replayed.
        assert!(store.take(id, Purpose::Archive).is_err());
    }

    #[test]
    fn malformed_handles_are_refused() {
        for id in ["", "0", "01", "-1", "1.0", "9223372036854775808"] {
            assert!(
                matches!(SelectionId::parse(id), Err(ref error) if error.code == Code::InvalidRequest),
                "{id}"
            );
        }
        assert!(SelectionId::parse("9007199254740993").is_ok());
    }

    #[test]
    fn record_count_stays_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = SelectionStore::new();
        let mut ids = Vec::new();
        for index in 0..=MAX_SELECTIONS {
            let archive = temp.path().join(format!("{index}.zip"));
            fs::write(&archive, b"not a zip").unwrap();
            ids.push(store.insert(Purpose::Archive, archive).unwrap());
        }
        assert!(store.take(ids[0], Purpose::Archive).is_err());
        assert!(
            store
                .take(ids.last().copied().unwrap(), Purpose::Archive)
                .is_ok()
        );
    }
}
