//! Portable root-identity evidence.
//!
//! Task 4 owns native binding, traversal and observation. This module currently
//! defines only the evidence record so staged payloads can name their owner.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeIdentity {
    Linux {
        fs_magic: u64,
        fs_id: [u8; 16],
        tree_id: Option<u64>,
        ino: u64,
        birth: (i64, u32),
    },
    Windows {
        volume_serial: u64,
        file_id: [u8; 16],
        creation_100ns: i64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootIdentity(NativeIdentity);

impl RootIdentity {
    pub fn from_adapter_evidence(identity: NativeIdentity) -> Self {
        Self(identity)
    }

    pub fn evidence(&self) -> &NativeIdentity {
        &self.0
    }
}
