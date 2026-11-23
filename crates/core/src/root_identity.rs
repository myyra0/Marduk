//! Portable root-identity evidence and root observation outcomes.
//!
//! Native adapters produce [`NativeIdentity`]; [`RootIdentity`] wraps it as an
//! opaque proof-token for persistence. Possessing the token never mints live
//! directory authority: binding, resolution and observation are platform calls.

use std::fmt;

use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeIdentity {
    Linux {
        fs_magic: u64,
        fs_id: [u8; 16],
        /// Mount view reported by Linux `statx`, not a filesystem identifier.
        view_id: u64,
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

    /// Deterministic key for the physical object this evidence describes.
    ///
    /// Used as the file name of the cross-catalog installation lease, so it is
    /// stable across reopen and distinct for sibling directories. The Linux
    /// mount view is excluded: another view of the same filesystem object is
    /// the same installation, and a lease must follow the object rather than
    /// the path it was reached through.
    pub fn installation_key(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(b"marduk-installation-lease/1");
        match self.0 {
            NativeIdentity::Linux {
                fs_magic,
                fs_id,
                tree_id,
                ino,
                birth,
                ..
            } => {
                hasher.update([0u8]);
                hasher.update(fs_magic.to_le_bytes());
                hasher.update(fs_id);
                match tree_id {
                    Some(id) => {
                        hasher.update([1u8]);
                        hasher.update(id.to_le_bytes());
                    }
                    None => hasher.update([0u8]),
                }
                hasher.update(ino.to_le_bytes());
                hasher.update(birth.0.to_le_bytes());
                hasher.update(birth.1.to_le_bytes());
            }
            NativeIdentity::Windows {
                volume_serial,
                file_id,
                creation_100ns,
            } => {
                hasher.update([1u8]);
                hasher.update(volume_serial.to_le_bytes());
                hasher.update(file_id);
                hasher.update(creation_100ns.to_le_bytes());
            }
        }
        hasher.finalize().into()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileMetadata {
    Linux {
        mode: u32,
        uid: u32,
        gid: u32,
        mtime: (i64, u32),
        nlink: u64,
    },
    Windows {
        attributes: u32,
        last_write_100ns: i64,
        links: u32,
        security_sha256: [u8; 32],
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fingerprint {
    pub identity: NativeIdentity,
    pub len: u64,
    pub sha256: [u8; 32],
    pub metadata: FileMetadata,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnsupportedReason {
    ObjectKind,
    Metadata,
    Sharing,
    Configuration,
}

impl fmt::Display for UnsupportedReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ObjectKind => "object kind",
            Self::Metadata => "metadata",
            Self::Sharing => "sharing",
            Self::Configuration => "configuration",
        })
    }
}

/// Typed root refusal. Variants carry no native paths or diagnostics, so both
/// `Debug` and `Display` stay redacted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootError {
    TargetUnavailable,
    MissingParent,
    AmbiguousName,
    OutsideBoundary,
    UnsupportedTarget(UnsupportedReason),
    Io,
}

impl fmt::Display for RootError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TargetUnavailable => formatter.write_str("the target root is unavailable"),
            Self::MissingParent => formatter.write_str("the target parent is missing"),
            Self::AmbiguousName => formatter.write_str("the target name is ambiguous"),
            Self::OutsideBoundary => formatter.write_str("the path leaves the verified root"),
            Self::UnsupportedTarget(reason) => {
                write!(formatter, "the target is unsupported: {reason}")
            }
            Self::Io => formatter.write_str("a native operation failed"),
        }
    }
}

impl std::error::Error for RootError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TargetObservation {
    Absent,
    Regular { fingerprint: Fingerprint },
    Unsupported { reason: UnsupportedReason },
}

/// Neutral game description consumed by the platform adapter. The adapter
/// never branches on a game name.
#[derive(Clone, Copy, Debug)]
pub struct GameLayout {
    pub data: &'static str,
    pub markers: &'static [&'static str],
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linux(ino: u64, view_id: u64, birth: (i64, u32)) -> RootIdentity {
        RootIdentity::from_adapter_evidence(NativeIdentity::Linux {
            fs_magic: 0x0102_1994,
            fs_id: [7u8; 16],
            view_id,
            tree_id: Some(256),
            ino,
            birth,
        })
    }

    fn windows(file_id: [u8; 16]) -> RootIdentity {
        RootIdentity::from_adapter_evidence(NativeIdentity::Windows {
            volume_serial: 0x1234_5678,
            file_id,
            creation_100ns: 133_000_000_000_000_000,
        })
    }

    #[test]
    fn the_same_evidence_gives_the_same_key() {
        assert_eq!(
            linux(42, 9, (1, 2)).installation_key(),
            linux(42, 9, (1, 2)).installation_key()
        );
        assert_eq!(
            windows([3u8; 16]).installation_key(),
            windows([3u8; 16]).installation_key()
        );
    }

    #[test]
    fn sibling_directories_get_different_keys() {
        assert_ne!(
            linux(42, 9, (1, 0)).installation_key(),
            linux(43, 9, (1, 0)).installation_key()
        );
        assert_ne!(
            windows([3u8; 16]).installation_key(),
            windows([4u8; 16]).installation_key()
        );
    }

    #[test]
    fn the_mount_view_does_not_change_the_key() {
        assert_eq!(
            linux(42, 9, (1, 0)).installation_key(),
            linux(42, 11, (1, 0)).installation_key()
        );
    }

    #[test]
    fn a_recreated_object_with_a_new_birth_gets_a_new_key() {
        assert_ne!(
            linux(42, 9, (1, 0)).installation_key(),
            linux(42, 9, (2, 0)).installation_key()
        );
    }
}
