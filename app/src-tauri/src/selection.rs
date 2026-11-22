use std::collections::BTreeMap;
use std::path::PathBuf;

use marduk_workspace::{Code, WireError};

/// Why a native selection was recorded. A handle from one picker is never
/// accepted by the other operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    Installation,
    Archive,
}

/// One-use, process-local selection handles. The renderer only ever sees the
/// decimal handle; the path stays in Rust and is revalidated by the owning
/// workspace module after it is consumed.
pub struct SelectionStore {
    next: u64,
    entries: BTreeMap<u64, Entry>,
}

struct Entry {
    purpose: Purpose,
    path: PathBuf,
}

/// Bounds process memory if a client keeps opening pickers without consuming.
const MAX_SELECTIONS: usize = 32;

impl SelectionStore {
    pub fn new() -> Self {
        Self {
            next: 1,
            entries: BTreeMap::new(),
        }
    }

    pub fn insert(&mut self, purpose: Purpose, path: PathBuf) -> String {
        if self.entries.len() >= MAX_SELECTIONS
            && let Some(oldest) = self.entries.keys().next().copied()
        {
            self.entries.remove(&oldest);
        }
        let id = self.next;
        self.next += 1;
        self.entries.insert(id, Entry { purpose, path });
        id.to_string()
    }

    pub fn take(&mut self, selection_id: &str, purpose: Purpose) -> Result<PathBuf, WireError> {
        let id = parse_selection_id(selection_id)?;
        let entry = self
            .entries
            .remove(&id)
            .ok_or_else(|| WireError::new(Code::InvalidRequest))?;
        if entry.purpose != purpose {
            return Err(WireError::new(Code::InvalidRequest));
        }
        Ok(entry.path)
    }
}

fn parse_selection_id(value: &str) -> Result<u64, WireError> {
    let digits = !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit());
    let canonical = value.len() == 1 || !value.starts_with('0');
    if !digits || !canonical {
        return Err(WireError::new(Code::InvalidRequest));
    }
    value
        .parse()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| WireError::new(Code::InvalidRequest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_are_single_use() {
        let mut store = SelectionStore::new();
        let id = store.insert(Purpose::Installation, PathBuf::from("/games/skyrim"));
        assert_eq!(
            store.take(&id, Purpose::Installation).unwrap(),
            PathBuf::from("/games/skyrim")
        );
        assert_eq!(
            store.take(&id, Purpose::Installation).unwrap_err(),
            WireError::new(Code::InvalidRequest)
        );
    }

    #[test]
    fn a_handle_is_bound_to_its_purpose() {
        let mut store = SelectionStore::new();
        let id = store.insert(Purpose::Archive, PathBuf::from("/tmp/a.zip"));
        assert_eq!(
            store.take(&id, Purpose::Installation).unwrap_err(),
            WireError::new(Code::InvalidRequest)
        );
        // Consumed even by the refused attempt, so it cannot be replayed.
        assert!(store.take(&id, Purpose::Archive).is_err());
    }

    #[test]
    fn malformed_handles_are_refused() {
        let mut store = SelectionStore::new();
        for id in ["", "0", "01", "-1", "1.0", "9007199254740993"] {
            assert!(
                matches!(store.take(id, Purpose::Installation), Err(ref error) if error.code == Code::InvalidRequest),
                "{id}"
            );
        }
    }

    #[test]
    fn record_count_stays_bounded() {
        let mut store = SelectionStore::new();
        let mut ids = Vec::new();
        for index in 0..=MAX_SELECTIONS {
            ids.push(store.insert(Purpose::Archive, PathBuf::from(format!("/tmp/{index}.zip"))));
        }
        assert!(store.take(&ids[0], Purpose::Archive).is_err());
        assert!(store.take(ids.last().unwrap(), Purpose::Archive).is_ok());
    }
}
