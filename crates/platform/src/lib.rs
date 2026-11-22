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

pub mod effects;
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

/// Whether the availability record proves private-library staging on the volume
/// holding `root`. `marduk_core::qualification` is the decision owner; this
/// function only observes the volume and asks. Unqualified volumes must not
/// stage.
pub fn library_volume_qualified(root: &Path) -> bool {
    native::library_volume_qualified(root)
}

/// Whether the availability record proves a client-visible deploy and restore on
/// the volume holding `root`. The decision owner is
/// `marduk_core::qualification`; this function only observes the volume and asks
/// for the client deploy route.
pub fn deploy_volume_qualified(root: &Path) -> bool {
    native::deploy_volume_qualified(root)
}

/// Open an application-owned child directory beneath the verified parent path,
/// re-proving each component, refusing a final link and every redirected
/// ancestor, and creating only that one missing child when absent.
pub fn open_private_child_directory(
    parent: &std::fs::File,
    name: &str,
) -> Result<std::fs::File, RootError> {
    native::open_private_child_directory(parent, name)
}

/// Open or create a chain of application-owned child directories beneath a
/// pinned parent, refusing a link at any component.
///
/// Each component is opened through the previously verified handle, so a
/// pre-existing symlink or junction is a typed refusal before the caller can
/// create or open an artifact beneath it.
pub fn open_private_child_chain(
    parent: &std::fs::File,
    components: &[&str],
) -> Result<std::fs::File, RootError> {
    let mut current = parent.try_clone().map_err(|_| RootError::Io)?;
    for name in components {
        current = native::open_private_child_directory(&current, name)?;
    }
    Ok(current)
}

/// Resolve a child pathname that stays beneath the pinned parent handle.
///
/// The parent must remain open for the returned path to keep its meaning.
pub fn private_child_path(
    parent: &std::fs::File,
    name: &str,
) -> Result<std::path::PathBuf, RootError> {
    native::private_child_path(parent, name)
}

/// Re-prove that `path` still resolves to the pinned directory handle. Callers
/// use this around a pathname-based open that cannot consume the handle.
pub fn verify_private_directory(path: &Path, handle: &std::fs::File) -> Result<(), RootError> {
    native::verify_private_directory(path, handle)
}

/// A regular file opened or created directly beneath a pinned app-owned
/// directory, with the leaf's native identity kept for later re-proof.
///
/// The held handle is the authority for the leaf's identity. Callers re-prove
/// the directory and this leaf around every pathname-based SQLite open. The
/// Linux leaf/sidecar substitution residual and the Windows rename residual are
/// owned by `docs/contracts/m1-recovery.md` (catalog file containment).
pub struct PrivateFile {
    inner: native::PrivateFile,
}

impl PrivateFile {
    /// A SQLite-compatible pathname that keeps the pinned directory as its
    /// first component. The parent handle must stay open.
    pub fn path(&self, parent: &std::fs::File) -> Result<std::path::PathBuf, RootError> {
        self.inner.path(parent)
    }

    /// Re-open the leaf without following a link and prove it is the same
    /// regular file on the pinned directory's volume.
    pub fn verify(&self, parent: &std::fs::File) -> Result<(), RootError> {
        self.inner.verify(parent)
    }
}

/// Open an existing regular child of the pinned directory, or create it with a
/// no-follow native handle. A reparse-tagged, non-regular or substituted leaf
/// is refused before the caller can write through a pathname.
pub fn open_or_create_private_file(
    parent: &std::fs::File,
    name: &str,
) -> Result<PrivateFile, RootError> {
    native::open_or_create_private_file(parent, name).map(|inner| PrivateFile { inner })
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

    /// Re-prove the live installation/data pair through its pinned handles. A
    /// renamed, replaced or redirected root is refused; no replacement binding
    /// is adopted.
    pub fn verify(&self) -> Result<(), RootError> {
        self.inner.check_roots()
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

    /// The target pathname rebuilt from the verified parent spelling and the
    /// resolved leaf. Callers must not rebuild it from a planned or archived
    /// spelling, which can name a different entry on a case-sensitive volume.
    pub fn public_path(&self) -> std::path::PathBuf {
        self.inner.public_path()
    }

    /// A clone of the verified parent directory handle for a handle-relative
    /// effect. The caller re-observes before acting; the handle keeps the
    /// parent pinned.
    pub fn parent_dir(&self) -> Result<std::fs::File, RootError> {
        self.inner.parent_dir()
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
