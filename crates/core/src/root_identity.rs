//! Portable root-identity evidence and root observation outcomes.
//!
//! Native adapters produce [`NativeIdentity`]; [`RootIdentity`] wraps it as an
//! opaque proof-token for persistence. Possessing the token never mints live
//! directory authority: binding, resolution and observation are platform calls.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeIdentity {
    Linux {
        /// Durable object identity. The mount view (`STATX_MNT_ID`) is a
        /// session property and stays out of the token; see ADR 0006.
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
