//! Platform primitives.
//!
//! The writer gate is the process-wide authority for mutating one library or
//! application data root. It is an active OS lock on a stable file, never a PID
//! file or a lock released by age. Roots and native root observation arrive in a
//! later task.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateError {
    /// Another entrant holds the gate.
    Busy,
    /// The lock object cannot be used: wrong type, extra links, replaced, or
    /// unavailable.
    Unavailable,
    /// The operating system failed for another reason.
    Io,
}

impl fmt::Display for GateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Busy => "another writer holds the gate",
            Self::Unavailable => "the lock object is unavailable",
            Self::Io => "the operating system failed",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for GateError {}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{WriterGate, WriterGuard};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{WriterGate, WriterGuard};
