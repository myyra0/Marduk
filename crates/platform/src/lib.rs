//! Platform primitives.
//!
//! The writer gate is the process-wide authority for mutating one library or
//! application data root. It is an active OS lock on a stable file, never a PID
//! file or a lock released by age.
//!
//! Root binding and observation are live, OS-owned directory handles. Portable
//! [`RootIdentity`] tokens are evidence for persistence; they cannot mint
//! authority, and neither handle type is serializable or rehydratable.

use std::fmt;
use std::path::Path;

use marduk_core::root_identity::{GameLayout, RootError, RootIdentity, TargetObservation};
use marduk_core::targets::RelativePath;

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

#[cfg(target_os = "linux")]
use linux as native;
#[cfg(windows)]
use windows as native;

pub mod library_stage;
#[doc(hidden)]
pub mod native_proof;

/// Open an application-owned directory without following a final link or
/// junction, creating it (and missing parents) only when it is absent.
///
/// The returned handle pins the verified directory. A path that already exists
/// as a symlink, junction or non-directory is refused and left untouched, so a
/// caller cannot be redirected into another tree before its first write.
pub fn open_private_directory(path: &Path) -> Result<std::fs::File, RootError> {
    native::open_private_directory(path)
}

/// A live, verified installation pair owned by the platform adapter.
///
/// The handles pin the selected installation and its direct data directory.
/// The type deliberately has no serialization or rehydration path.
pub struct InstallationBinding {
    inner: native::Binding,
}

impl InstallationBinding {
    /// Installation identity and direct-data identity, in that order.
    pub fn identities(&self) -> (&RootIdentity, &RootIdentity) {
        self.inner.identities()
    }
}

impl fmt::Debug for InstallationBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("InstallationBinding(..)")
    }
}

/// A bounded parent handle plus the one target spelling below it.
pub struct ResolvedTarget<'a> {
    inner: native::ResolvedTarget<'a>,
}

impl ResolvedTarget<'_> {
    pub fn parent_identity(&self) -> &RootIdentity {
        self.inner.parent_identity()
    }

    /// The existing spelling when exactly one name matches, otherwise the
    /// planned spelling supplied by the caller.
    pub fn leaf(&self) -> &str {
        self.inner.leaf()
    }
}

impl fmt::Debug for ResolvedTarget<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ResolvedTarget(..)")
    }
}

/// Open the selected installation and its direct data directory, and require
/// at least one inert regular marker. Not permission to write.
pub fn bind_installation(
    selection: &Path,
    layout: &GameLayout,
) -> Result<InstallationBinding, RootError> {
    native::bind_installation(selection, layout).map(|inner| InstallationBinding { inner })
}

/// Reopen both recorded directories and verify the exact identity pair again.
/// Missing, moved or substituted roots are refused; no replacement is adopted.
pub fn reopen_installation(
    location: &Path,
    layout: &GameLayout,
    installation: &RootIdentity,
    data: &RootIdentity,
) -> Result<InstallationBinding, RootError> {
    native::reopen_installation(location, layout, installation, data)
        .map(|inner| InstallationBinding { inner })
}

/// Resolve every parent component under the verified data root and retain the
/// single matching leaf spelling, or the planned spelling when nothing exists.
pub fn resolve_existing_parent<'a>(
    binding: &'a InstallationBinding,
    path: &RelativePath,
) -> Result<ResolvedTarget<'a>, RootError> {
    native::resolve_existing_parent(&binding.inner, path).map(|inner| ResolvedTarget { inner })
}

/// Observe the resolved leaf without following links. `Absent` only ever means
/// an already-authorized parent has no matching target.
pub fn observe_target(target: &ResolvedTarget<'_>) -> Result<TargetObservation, RootError> {
    native::observe_target(&target.inner)
}
