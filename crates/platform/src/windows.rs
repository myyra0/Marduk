//! Windows writer gate: a kept `CreateFileW` handle locking byte zero.

use std::fs::File;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION, GENERIC_READ,
    GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE, WIN32_ERROR,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
    GetFileInformationByHandle, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
    OPEN_ALWAYS, OPEN_EXISTING, UnlockFileEx,
};
use windows_sys::Win32::System::IO::OVERLAPPED;

use crate::GateError;

/// Stable lock object for one application-owned root.
#[derive(Debug)]
pub struct WriterGate {
    path: PathBuf,
    anchor: Option<(File, String)>,
    volume: u32,
    index: u64,
}

/// Held exclusive lock. The handle stays open, so the byte-range lock is held.
#[derive(Debug)]
pub struct WriterGuard {
    file: File,
}

impl WriterGate {
    /// Open or create the lock file and remember its identity. The parent
    /// directory must already exist and be owned by the application.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, GateError> {
        let path = path.as_ref().to_path_buf();
        let file = open_lock_file(&path, true)?;
        let (volume, index) = identify(&file)?;
        Ok(Self {
            path,
            anchor: None,
            volume,
            index,
        })
    }

    /// Open or create the lock file through a pinned directory handle. The
    /// pathname is rebuilt from that handle on every lock, so a substituted
    /// ancestor cannot redirect the lock object.
    pub fn open_at(parent: &File, name: &str) -> Result<Self, GateError> {
        let path = root::private_child_path(parent, name).map_err(|_| GateError::Unavailable)?;
        let file = open_lock_file(&path, true)?;
        let (volume, index) = identify(&file)?;
        let anchor = parent.try_clone().map_err(|_| GateError::Io)?;
        Ok(Self {
            path,
            anchor: Some((anchor, name.to_owned())),
            volume,
            index,
        })
    }

    /// Take the exclusive lock for this entrant.
    ///
    /// There is no delete sharing, so another entrant cannot replace the lock
    /// file while it is held. Each entrant opens its own handle.
    pub fn try_lock(&self) -> Result<WriterGuard, GateError> {
        let path = match &self.anchor {
            Some((parent, name)) => {
                root::private_child_path(parent, name).map_err(|_| GateError::Unavailable)?
            }
            None => self.path.clone(),
        };
        let file = open_lock_file(&path, false)?;
        if identify(&file)? != (self.volume, self.index) {
            return Err(GateError::Unavailable);
        }

        let mut overlapped = OVERLAPPED::default();
        let result = unsafe {
            LockFileEx(
                file.as_raw_handle() as HANDLE,
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                1,
                0,
                &mut overlapped,
            )
        };
        if result != 0 {
            return Ok(WriterGuard { file });
        }

        match unsafe { GetLastError() } {
            ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION => Err(GateError::Busy),
            _ => Err(GateError::Io),
        }
    }
}

impl Drop for WriterGuard {
    fn drop(&mut self) {
        let mut overlapped = OVERLAPPED::default();
        unsafe {
            UnlockFileEx(
                self.file.as_raw_handle() as HANDLE,
                0,
                1,
                0,
                &mut overlapped,
            );
        }
    }
}

fn open_lock_file(path: &Path, create: bool) -> Result<File, GateError> {
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            if create { OPEN_ALWAYS } else { OPEN_EXISTING },
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(open_error(unsafe { GetLastError() }));
    }
    Ok(unsafe { File::from_raw_handle(handle) })
}

fn identify(file: &File) -> Result<(u32, u64), GateError> {
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut information) } == 0
    {
        return Err(open_error(unsafe { GetLastError() }));
    }

    if information.dwFileAttributes & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DIRECTORY) != 0
        || information.nNumberOfLinks != 1
    {
        return Err(GateError::Unavailable);
    }

    let index =
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow);
    Ok((information.dwVolumeSerialNumber, index))
}

fn open_error(code: WIN32_ERROR) -> GateError {
    match code {
        ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION => GateError::Busy,
        _ => GateError::Io,
    }
}

// ---------------------------------------------------------------------------
// Native root binding and observation.
//
// Each child is opened no-follow with a GUID path derived from its pinned
// parent and verified against that parent's native enumeration. Directory
// handles deny write and delete sharing from the volume root through the
// resolved parent. Observation is not write authority.
// ---------------------------------------------------------------------------
mod root {
    use super::ERROR_ALREADY_EXISTS;
    use std::fs::File;
    use std::io::Read;
    use std::mem::offset_of;
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use std::path::{Component, Path, PathBuf, Prefix};
    use std::ptr;
    use std::sync::Mutex;

    use marduk_core::root_identity::{
        FileMetadata, Fingerprint, GameLayout, NativeIdentity, RootError, RootIdentity,
        TargetObservation, UnsupportedReason,
    };
    use marduk_core::targets::RelativePath;
    use sha2::{Digest, Sha256};
    use windows_sys::Wdk::Storage::FileSystem::{
        FILE_EA_INFORMATION, FileEaInformation, NtQueryInformationFile,
    };
    use windows_sys::Win32::Foundation::{
        ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_HANDLE_EOF, ERROR_INSUFFICIENT_BUFFER,
        ERROR_LOCK_VIOLATION, ERROR_MORE_DATA, ERROR_NO_MORE_FILES, ERROR_PATH_NOT_FOUND,
        ERROR_SHARING_VIOLATION, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE,
        INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Security::{
        ACL, ATTRIBUTE_SECURITY_INFORMATION, DACL_SECURITY_INFORMATION, GROUP_SECURITY_INFORMATION,
        GetKernelObjectSecurity, GetSecurityDescriptorDacl, GetSecurityDescriptorGroup,
        GetSecurityDescriptorOwner, GetSecurityDescriptorSacl, IsValidAcl, IsValidSid,
        LABEL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, SCOPE_SECURITY_INFORMATION,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CREATE_NEW, CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_ARCHIVE,
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_NORMAL,
        FILE_ATTRIBUTE_NOT_CONTENT_INDEXED, FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_ATTRIBUTE_SYSTEM, FILE_ATTRIBUTE_TAG_INFO, FILE_BASIC_INFO,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_EXTD_DIR_INFO,
        FILE_ID_INFO, FILE_INFO_BY_HANDLE_CLASS, FILE_LIST_DIRECTORY, FILE_NAME_NORMALIZED,
        FILE_READ_ATTRIBUTES, FILE_READ_EA, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        FILE_STANDARD_INFO, FILE_STREAM_INFO, FileAttributeTagInfo, FileBasicInfo,
        FileIdExtdDirectoryInfo, FileIdExtdDirectoryRestartInfo, FileIdInfo, FileStandardInfo,
        FileStreamInfo, FindClose, FindFirstFileW, GetFileInformationByHandleEx,
        GetFinalPathNameByHandleW, GetVolumeInformationByHandleW,
        GetVolumeNameForVolumeMountPointW, OPEN_EXISTING, READ_CONTROL, ReOpenFile,
        VOLUME_NAME_GUID, WIN32_FIND_DATAW,
    };
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

    const NO_FOLLOW: u32 = FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT;
    const DIRECTORY_ACCESS: u32 = FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES;
    const METADATA_ACCESS: u32 = FILE_READ_ATTRIBUTES | FILE_READ_EA | READ_CONTROL;
    const ORDINARY_ATTRIBUTES: u32 = FILE_ATTRIBUTE_ARCHIVE
        | FILE_ATTRIBUTE_HIDDEN
        | FILE_ATTRIBUTE_NORMAL
        | FILE_ATTRIBUTE_NOT_CONTENT_INDEXED
        | FILE_ATTRIBUTE_READONLY
        | FILE_ATTRIBUTE_SYSTEM;

    pub(crate) struct RootDir {
        file: File,
        final_path: String,
        volume: u64,
        identity: RootIdentity,
    }

    pub(crate) struct Binding {
        ancestors: Vec<RootDir>,
        installation: RootDir,
        data: RootDir,
    }

    impl Binding {
        pub(crate) fn identities(&self) -> (&RootIdentity, &RootIdentity) {
            (&self.installation.identity, &self.data.identity)
        }

        pub(crate) fn check_roots(&self) -> Result<(), RootError> {
            for root in self
                .ancestors
                .iter()
                .chain([&self.installation, &self.data])
            {
                check_locator(root)?;
            }
            Ok(())
        }
    }

    #[derive(Clone)]
    struct DirectoryEntry {
        name: String,
        attributes: u32,
        reparse_tag: u32,
        file_id: [u8; 16],
        creation_100ns: i64,
    }

    impl DirectoryEntry {
        fn is_reparse(&self) -> bool {
            self.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 || self.reparse_tag != 0
        }
    }

    pub(crate) struct ResolvedTarget<'a> {
        parents: Vec<RootDir>,
        leaf: String,
        logical_leaf: String,
        /// Identity enumerated from the pinned parent when the leaf existed.
        leaf_identity: Option<NativeIdentity>,
        binding: &'a Binding,
    }

    impl ResolvedTarget<'_> {
        fn parent(&self) -> &RootDir {
            self.parents.last().unwrap_or(&self.binding.data)
        }

        fn check_chain(&self) -> Result<(), RootError> {
            for root in self
                .binding
                .ancestors
                .iter()
                .chain([&self.binding.installation, &self.binding.data])
                .chain(&self.parents)
            {
                check_locator(root)?;
            }
            Ok(())
        }

        pub(crate) fn parent_identity(&self) -> &RootIdentity {
            &self.parent().identity
        }

        pub(crate) fn parent_dir(&self) -> Result<File, RootError> {
            self.parent().file.try_clone().map_err(|_| RootError::Io)
        }

        pub(crate) fn leaf(&self) -> &str {
            &self.leaf
        }
    }

    pub(crate) fn bind_installation(
        selection: &Path,
        layout: &GameLayout,
    ) -> Result<Binding, RootError> {
        let binding = bind_pair(selection, layout)?;
        if !has_marker(&binding.installation, layout)? {
            return Err(RootError::UnsupportedTarget(
                UnsupportedReason::Configuration,
            ));
        }

        Ok(binding)
    }

    pub(crate) fn reopen_installation(
        location: &Path,
        layout: &GameLayout,
        installation: &RootIdentity,
        data: &RootIdentity,
    ) -> Result<Binding, RootError> {
        let reopened = bind_pair(location, layout).map_err(|_| RootError::TargetUnavailable)?;
        if reopened.installation.identity != *installation || reopened.data.identity != *data {
            return Err(RootError::TargetUnavailable);
        }
        Ok(reopened)
    }

    fn bind_pair(location: &Path, layout: &GameLayout) -> Result<Binding, RootError> {
        let mut ancestors = open_selection(location, false)?;
        let installation = ancestors.pop().expect("volume root is pinned");
        let data = pin_directory(&installation, layout.data)
            .map_err(|_| RootError::UnsupportedTarget(UnsupportedReason::Configuration))?;
        Ok(Binding {
            ancestors,
            installation,
            data,
        })
    }

    pub(crate) fn resolve_existing_parent<'a>(
        binding: &'a Binding,
        path: &RelativePath,
    ) -> Result<ResolvedTarget<'a>, RootError> {
        let mut components: Vec<&str> = path.as_str().split('/').collect();
        let leaf_planned = components
            .pop()
            .expect("validated path has a component")
            .to_owned();

        let mut parents = Vec::new();

        for component in components {
            let child = pin_directory(parents.last().unwrap_or(&binding.data), component)?;
            parents.push(child);
        }

        let parent = parents.last().unwrap_or(&binding.data);
        let (leaf, leaf_identity) = match leaf_match(parent, &leaf_planned)? {
            Some(entry) => {
                if entry.is_reparse() {
                    return Err(RootError::OutsideBoundary);
                }
                open_entry(parent, &entry, FILE_READ_ATTRIBUTES)?;
                let expected = entry_identity(parent.volume, &entry);
                (entry.name, Some(expected))
            }
            None => (leaf_planned.clone(), None),
        };

        Ok(ResolvedTarget {
            parents,
            leaf,
            logical_leaf: leaf_planned,
            leaf_identity,
            binding,
        })
    }

    pub(crate) fn observe_target(
        target: &ResolvedTarget<'_>,
    ) -> Result<TargetObservation, RootError> {
        let parent = target.parent();
        target.check_chain()?;
        let Some(entry) = leaf_match(parent, &target.logical_leaf)? else {
            target.check_chain()?;
            return Ok(TargetObservation::Absent);
        };
        let enumerated = entry_identity(parent.volume, &entry);
        if target
            .leaf_identity
            .as_ref()
            .is_some_and(|expected| *expected != enumerated)
        {
            return Err(RootError::TargetUnavailable);
        }

        if entry.is_reparse() || entry.attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
            return Ok(TargetObservation::Unsupported {
                reason: UnsupportedReason::ObjectKind,
            });
        }
        let metadata = match open_entry(parent, &entry, METADATA_ACCESS) {
            Ok(file) => file,
            Err(RootError::UnsupportedTarget(reason)) => {
                return Ok(TargetObservation::Unsupported { reason });
            }
            Err(error) => return Err(error),
        };
        let standard = standard_info(&metadata)?;
        if is_reparse(&attribute_tag(&metadata)?) || standard.Directory {
            return Ok(TargetObservation::Unsupported {
                reason: UnsupportedReason::ObjectKind,
            });
        }
        let before_basic = basic_info(&metadata)?;
        let Some(security_sha256) = eligible_metadata(&metadata, &before_basic, &standard) else {
            return Ok(TargetObservation::Unsupported {
                reason: UnsupportedReason::Metadata,
            });
        };

        // Open the already verified object for data, never its pathname again.
        let handle = unsafe {
            ReOpenFile(
                metadata.as_raw_handle(),
                GENERIC_READ,
                FILE_SHARE_READ,
                NO_FOLLOW,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            let code = unsafe { GetLastError() };
            if is_sharing(code) {
                return Ok(TargetObservation::Unsupported {
                    reason: UnsupportedReason::Sharing,
                });
            }
            return Err(RootError::TargetUnavailable);
        }
        let file = unsafe { File::from_raw_handle(handle) };
        check_original(&file, &enumerated, &before_basic, &standard)?;
        if eligible_metadata(&file, &before_basic, &standard) != Some(security_sha256) {
            return Ok(TargetObservation::Unsupported {
                reason: UnsupportedReason::Metadata,
            });
        }

        let fingerprint = match fingerprint_open_file(&file) {
            Ok(fingerprint) => fingerprint,
            Err(RootError::UnsupportedTarget(reason)) => {
                return Ok(TargetObservation::Unsupported { reason });
            }
            Err(error) => return Err(error),
        };
        if fingerprint.identity != enumerated {
            return Err(RootError::TargetUnavailable);
        }
        // The public leaf is checked again after the read so the fingerprint
        // still belongs to the authorized name.
        let current =
            leaf_match(parent, &target.logical_leaf)?.ok_or(RootError::TargetUnavailable)?;
        if entry_identity(parent.volume, &current) != enumerated {
            return Err(RootError::TargetUnavailable);
        }
        target.check_chain()?;

        Ok(TargetObservation::Regular { fingerprint })
    }

    fn open_selection(path: &Path, create: bool) -> Result<Vec<RootDir>, RootError> {
        if path.components().any(|part| part == Component::ParentDir) {
            return Err(RootError::TargetUnavailable);
        }
        let absolute = std::path::absolute(path).map_err(|_| RootError::TargetUnavailable)?;
        let mut parts = absolute.components();
        let Some(Component::Prefix(prefix)) = parts.next() else {
            return Err(RootError::TargetUnavailable);
        };
        if parts.next() != Some(Component::RootDir) {
            return Err(RootError::TargetUnavailable);
        }
        let guid = match prefix.kind() {
            Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => {
                let drive = [drive as u16, b':' as u16, b'\\' as u16, 0];
                let mut guid = [0u16; 50];
                if unsafe {
                    GetVolumeNameForVolumeMountPointW(
                        drive.as_ptr(),
                        guid.as_mut_ptr(),
                        guid.len() as u32,
                    )
                } == 0
                {
                    return Err(RootError::TargetUnavailable);
                }
                let end = guid
                    .iter()
                    .position(|unit| *unit == 0)
                    .ok_or(RootError::TargetUnavailable)?;
                String::from_utf16(&guid[..end]).map_err(|_| RootError::TargetUnavailable)?
            }
            Prefix::Verbatim(name)
                if name.to_str().is_some_and(|name| {
                    name.len() == 44
                        && name
                            .get(..7)
                            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("Volume{"))
                        && name.ends_with('}')
                }) =>
            {
                format!(
                    "{}\\",
                    prefix
                        .as_os_str()
                        .to_str()
                        .ok_or(RootError::TargetUnavailable)?
                )
            }
            _ => return Err(RootError::TargetUnavailable), // network/device namespaces are not local volumes
        };
        let root = open_path(&guid, DIRECTORY_ACCESS).map_err(|_| RootError::TargetUnavailable)?;
        verify_volume_filesystem(&root)?;
        let pinned = verify_root(root)?;
        if !pinned.final_path.eq_ignore_ascii_case(&guid) {
            return Err(RootError::TargetUnavailable);
        }
        let mut chain = vec![pinned];
        for part in parts {
            let Component::Normal(name) = part else {
                return Err(RootError::TargetUnavailable);
            };
            let name = name.to_str().ok_or(RootError::TargetUnavailable)?;
            let parent = chain.last().expect("volume root is pinned");
            match pin_selected_directory(parent, name) {
                Ok(child) => chain.push(child),
                Err(RootError::TargetUnavailable) if create => {
                    if leaf_match(parent, name)?.is_some() {
                        return Err(RootError::TargetUnavailable);
                    }
                    create_directory(parent, name)?;
                    chain.push(pin_selected_directory(parent, name)?);
                }
                Err(error) => return Err(error),
            }
        }
        Ok(chain)
    }

    fn create_directory(parent: &RootDir, name: &str) -> Result<(), RootError> {
        check_locator(parent)?;
        let wide: Vec<u16> = join(&parent.final_path, name)
            .encode_utf16()
            .chain([0])
            .collect();
        if unsafe { CreateDirectoryW(wide.as_ptr(), ptr::null()) } == 0 {
            let code = unsafe { GetLastError() };
            if code != ERROR_ALREADY_EXISTS {
                return Err(RootError::Io);
            }
        }
        Ok(())
    }

    fn open_path(path: &str, access: u32) -> Result<File, u32> {
        let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                access,
                FILE_SHARE_READ,
                ptr::null(),
                OPEN_EXISTING,
                NO_FOLLOW,
                ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(unsafe { GetLastError() });
        }
        Ok(unsafe { File::from_raw_handle(handle) })
    }

    fn open_entry(
        parent: &RootDir,
        entry: &DirectoryEntry,
        access: u32,
    ) -> Result<File, RootError> {
        check_locator(parent)?;
        let file = open_path(&join(&parent.final_path, &entry.name), access).map_err(|code| {
            if matches!(code, ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND) {
                RootError::TargetUnavailable
            } else if is_sharing(code) {
                RootError::UnsupportedTarget(UnsupportedReason::Sharing)
            } else {
                RootError::Io
            }
        })?;
        if identity(&file)? != entry_identity(parent.volume, entry) {
            return Err(RootError::TargetUnavailable);
        }
        check_locator(parent)?;
        Ok(file)
    }

    fn pin_directory(parent: &RootDir, name: &str) -> Result<RootDir, RootError> {
        let entry = leaf_match(parent, name)?.ok_or(RootError::MissingParent)?;
        if entry.is_reparse() {
            return Err(RootError::OutsideBoundary);
        }
        if entry.attributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
            return Err(RootError::UnsupportedTarget(UnsupportedReason::ObjectKind));
        }
        let file = open_entry(parent, &entry, DIRECTORY_ACCESS)?;
        let child = verify_root(file)?;
        if child.final_path != join(&parent.final_path, &entry.name)
            || child.volume != parent.volume
            || child.identity == parent.identity
        {
            return Err(RootError::OutsideBoundary);
        }
        Ok(child)
    }

    fn pin_selected_directory(parent: &RootDir, name: &str) -> Result<RootDir, RootError> {
        if !ordinary_component(name) {
            return Err(RootError::TargetUnavailable);
        }
        check_locator(parent).map_err(|_| RootError::TargetUnavailable)?;
        // Canonicalize one component at a time. A single open of the whole
        // selection with OPEN_REPARSE_POINT only protects its final component.
        let file = open_path(&join(&parent.final_path, name), DIRECTORY_ACCESS)
            .map_err(|_| RootError::TargetUnavailable)?;
        let child = verify_root(file).map_err(|_| RootError::TargetUnavailable)?;
        let canonical = child
            .final_path
            .strip_prefix(&join(&parent.final_path, ""))
            .filter(|name| ordinary_component(name))
            .ok_or(RootError::TargetUnavailable)?;
        let entry = leaf_match(parent, canonical)
            .map_err(|_| RootError::TargetUnavailable)?
            .ok_or(RootError::TargetUnavailable)?;
        if entry.name != canonical
            || entry.is_reparse()
            || entry.attributes & FILE_ATTRIBUTE_DIRECTORY == 0
            || entry_identity(parent.volume, &entry) != *child.identity.evidence()
            || child.identity == parent.identity
        {
            return Err(RootError::TargetUnavailable);
        }
        Ok(child)
    }

    fn join(parent: &str, name: &str) -> String {
        format!("{}\\{}", parent.trim_end_matches('\\'), name)
    }

    fn verify_root(file: File) -> Result<RootDir, RootError> {
        let tag = attribute_tag(&file)?;
        if is_reparse(&tag) {
            return Err(RootError::TargetUnavailable);
        }
        let standard = standard_info(&file)?;
        if !standard.Directory {
            return Err(RootError::TargetUnavailable);
        }
        let final_path = final_path(&file)?;
        let id = file_id(&file)?;
        let identity = RootIdentity::from_adapter_evidence(identity(&file)?);
        Ok(RootDir {
            file,
            final_path,
            volume: id.VolumeSerialNumber,
            identity,
        })
    }

    fn check_locator(root: &RootDir) -> Result<(), RootError> {
        if final_path(&root.file).map_err(|_| RootError::TargetUnavailable)? != root.final_path
            || is_reparse(&attribute_tag(&root.file)?)
        {
            return Err(RootError::TargetUnavailable);
        }
        Ok(())
    }

    fn identity(file: &File) -> Result<NativeIdentity, RootError> {
        let id = file_id(file)?;
        let basic = basic_info(file)?;
        if id.VolumeSerialNumber == 0 || id.FileId.Identifier == [0; 16] || basic.CreationTime == 0
        {
            return Err(RootError::UnsupportedTarget(
                UnsupportedReason::Configuration,
            ));
        }
        Ok(NativeIdentity::Windows {
            volume_serial: id.VolumeSerialNumber,
            file_id: id.FileId.Identifier,
            creation_100ns: basic.CreationTime,
        })
    }

    fn file_id(file: &File) -> Result<FILE_ID_INFO, RootError> {
        let mut info = FILE_ID_INFO::default();
        if unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle() as HANDLE,
                FileIdInfo,
                (&mut info as *mut FILE_ID_INFO).cast(),
                size_of::<FILE_ID_INFO>() as u32,
            )
        } == 0
        {
            return Err(RootError::Io);
        }
        Ok(info)
    }

    fn basic_info(file: &File) -> Result<FILE_BASIC_INFO, RootError> {
        let mut info = FILE_BASIC_INFO::default();
        if unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle() as HANDLE,
                FileBasicInfo,
                (&mut info as *mut FILE_BASIC_INFO).cast(),
                size_of::<FILE_BASIC_INFO>() as u32,
            )
        } == 0
        {
            return Err(RootError::Io);
        }
        Ok(info)
    }

    fn standard_info(file: &File) -> Result<FILE_STANDARD_INFO, RootError> {
        let mut info = FILE_STANDARD_INFO::default();
        if unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle() as HANDLE,
                FileStandardInfo,
                (&mut info as *mut FILE_STANDARD_INFO).cast(),
                size_of::<FILE_STANDARD_INFO>() as u32,
            )
        } == 0
        {
            return Err(RootError::Io);
        }
        Ok(info)
    }

    fn attribute_tag(file: &File) -> Result<FILE_ATTRIBUTE_TAG_INFO, RootError> {
        let mut info = FILE_ATTRIBUTE_TAG_INFO::default();
        if unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle() as HANDLE,
                FileAttributeTagInfo,
                (&mut info as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
                size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
            )
        } == 0
        {
            return Err(RootError::Io);
        }
        Ok(info)
    }

    fn is_reparse(tag: &FILE_ATTRIBUTE_TAG_INFO) -> bool {
        tag.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 || tag.ReparseTag != 0
    }

    fn verify_volume_filesystem(file: &File) -> Result<(), RootError> {
        let mut name = [0u16; 64];
        let result = unsafe {
            GetVolumeInformationByHandleW(
                file.as_raw_handle() as HANDLE,
                ptr::null_mut(),
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                name.as_mut_ptr(),
                name.len() as u32,
            )
        };
        if result == 0 {
            return Err(RootError::Io);
        }
        let end = name
            .iter()
            .position(|unit| *unit == 0)
            .filter(|end| *end != 0)
            .ok_or(RootError::UnsupportedTarget(
                UnsupportedReason::Configuration,
            ))?;
        char::decode_utf16(name[..end].iter().copied())
            .try_for_each(|unit| unit.map(|_| ()).map_err(|_| RootError::Io))
    }

    fn final_path(file: &File) -> Result<String, RootError> {
        let mut buffer = vec![0u16; 512];
        loop {
            let len = unsafe {
                GetFinalPathNameByHandleW(
                    file.as_raw_handle() as HANDLE,
                    buffer.as_mut_ptr(),
                    buffer.len() as u32,
                    FILE_NAME_NORMALIZED | VOLUME_NAME_GUID,
                )
            };
            if len == 0 {
                return Err(RootError::Io);
            }
            if (len as usize) < buffer.len() {
                buffer.truncate(len as usize);
                return String::from_utf16(&buffer).map_err(|_| RootError::TargetUnavailable);
            }
            buffer = vec![0u16; len as usize + 1];
        }
    }

    fn read_directory(directory: &File) -> Result<Vec<DirectoryEntry>, RootError> {
        // Restart and continue calls share the directory handle's cursor.
        static ENUMERATION: Mutex<()> = Mutex::new(());
        let _guard = ENUMERATION.lock().map_err(|_| RootError::Io)?;
        let mut buffer = vec![0u8; 256 * 1024];
        loop {
            let mut entries = Vec::new();
            let mut restart = true;
            let mut failed = None;
            loop {
                let class: FILE_INFO_BY_HANDLE_CLASS = if restart {
                    FileIdExtdDirectoryRestartInfo
                } else {
                    FileIdExtdDirectoryInfo
                };
                let ok = unsafe {
                    GetFileInformationByHandleEx(
                        directory.as_raw_handle() as HANDLE,
                        class,
                        buffer.as_mut_ptr().cast(),
                        buffer.len() as u32,
                    )
                };
                if ok == 0 {
                    let code = unsafe { GetLastError() };
                    if code == ERROR_NO_MORE_FILES || code == ERROR_HANDLE_EOF {
                        return Ok(entries);
                    }
                    failed = Some(code);
                    break;
                }
                restart = false;

                let mut offset = 0usize;
                loop {
                    // The unaligned read below covers the whole fixed record,
                    // not just the part before the variable-length name.
                    if offset
                        .checked_add(size_of::<FILE_ID_EXTD_DIR_INFO>())
                        .is_none_or(|end| end > buffer.len())
                    {
                        failed = Some(ERROR_INSUFFICIENT_BUFFER);
                        break;
                    }
                    let info = unsafe {
                        ptr::read_unaligned(
                            buffer.as_ptr().add(offset) as *const FILE_ID_EXTD_DIR_INFO
                        )
                    };
                    let name_offset = offset + offset_of!(FILE_ID_EXTD_DIR_INFO, FileName);
                    let units = info.FileNameLength as usize / 2;
                    let name_bytes = match units.checked_mul(2) {
                        Some(bytes) => bytes,
                        None => return Err(RootError::Io),
                    };
                    if name_offset
                        .checked_add(name_bytes)
                        .is_none_or(|end| end > buffer.len())
                    {
                        failed = Some(ERROR_INSUFFICIENT_BUFFER);
                        break;
                    }
                    let name = unsafe {
                        std::slice::from_raw_parts(
                            buffer.as_ptr().add(name_offset).cast::<u16>(),
                            units,
                        )
                    };
                    if let Ok(name) = String::from_utf16(name)
                        && name != "."
                        && name != ".."
                    {
                        entries.push(DirectoryEntry {
                            name,
                            attributes: info.FileAttributes,
                            reparse_tag: info.ReparsePointTag,
                            file_id: info.FileId.Identifier,
                            creation_100ns: info.CreationTime,
                        });
                    }
                    if info.NextEntryOffset == 0 {
                        break;
                    }
                    offset = match offset.checked_add(info.NextEntryOffset as usize) {
                        Some(next) => next,
                        None => return Err(RootError::Io),
                    };
                }
                if failed.is_some() {
                    break;
                }
            }

            if failed == Some(ERROR_INSUFFICIENT_BUFFER) && buffer.len() < 4 * 1024 * 1024 {
                buffer.resize(buffer.len() * 2, 0);
                continue;
            }
            return Err(RootError::Io);
        }
    }

    fn leaf_match(parent: &RootDir, component: &str) -> Result<Option<DirectoryEntry>, RootError> {
        if !ordinary_component(component) {
            return Err(RootError::TargetUnavailable);
        }
        check_locator(parent)?;
        let mut found = read_directory(&parent.file)?
            .into_iter()
            .filter(|entry| entry.name.eq_ignore_ascii_case(component));
        let entry = found.next();
        if found.next().is_some() {
            return Err(RootError::AmbiguousName);
        }
        // FindFirstFileW resolves a native lookup (including alternate 8.3
        // names) without requiring access to the target data or its ACL.
        let path: Vec<u16> = join(&parent.final_path, component)
            .encode_utf16()
            .chain([0])
            .collect();
        let mut found = WIN32_FIND_DATAW::default();
        let handle = unsafe { FindFirstFileW(path.as_ptr(), &mut found) };
        if handle == INVALID_HANDLE_VALUE {
            match unsafe { GetLastError() } {
                ERROR_FILE_NOT_FOUND
                    if entry.as_ref().is_none_or(|entry| entry.name != component) => {}
                ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => {
                    return Err(RootError::TargetUnavailable);
                }
                _ => return Err(RootError::Io),
            }
        } else {
            unsafe { FindClose(handle) };
            let end = found
                .cFileName
                .iter()
                .position(|unit| *unit == 0)
                .ok_or(RootError::Io)?;
            if entry.as_ref().is_none_or(|entry| {
                !entry
                    .name
                    .encode_utf16()
                    .eq(found.cFileName[..end].iter().copied())
            }) {
                return Err(RootError::AmbiguousName);
            }
        }
        check_locator(parent)?;
        Ok(entry)
    }

    fn entry_identity(volume: u64, entry: &DirectoryEntry) -> NativeIdentity {
        NativeIdentity::Windows {
            volume_serial: volume,
            file_id: entry.file_id,
            creation_100ns: entry.creation_100ns,
        }
    }

    fn has_marker(installation: &RootDir, layout: &GameLayout) -> Result<bool, RootError> {
        // Name, attributes and reparse state come from the pinned installation
        // directory enumeration; no game binary is opened.
        let entries = read_directory(&installation.file)?;
        for marker in layout.markers {
            if !ordinary_component(marker) {
                continue;
            }
            if entries.iter().any(|entry| {
                entry.name.eq_ignore_ascii_case(marker)
                    && entry.attributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT)
                        == 0
                    && entry.reparse_tag == 0
            }) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn ordinary_component(name: &str) -> bool {
        !name.is_empty()
            && name != "."
            && name != ".."
            && !name.contains('/')
            && !name.contains('\\')
            && !name.contains('\0')
            && !name.contains(':')
            && !name.contains(['*', '?'])
    }

    fn check_original(
        file: &File,
        expected: &NativeIdentity,
        basic: &FILE_BASIC_INFO,
        standard: &FILE_STANDARD_INFO,
    ) -> Result<(), RootError> {
        let current = basic_info(file)?;
        let current_standard = standard_info(file)?;
        if identity(file)? != *expected
            || is_reparse(&attribute_tag(file)?)
            || current.FileAttributes != basic.FileAttributes
            || current.LastWriteTime != basic.LastWriteTime
            || current.ChangeTime != basic.ChangeTime
            || current_standard.DeletePending
            || current_standard.Directory
            || current_standard.NumberOfLinks != standard.NumberOfLinks
            || current_standard.EndOfFile != standard.EndOfFile
        {
            return Err(RootError::TargetUnavailable);
        }
        Ok(())
    }

    fn eligible_metadata(
        file: &File,
        basic: &FILE_BASIC_INFO,
        standard: &FILE_STANDARD_INFO,
    ) -> Option<[u8; 32]> {
        if standard.DeletePending
            || standard.NumberOfLinks != 1
            || standard.EndOfFile < 0
            || basic.FileAttributes & !ORDINARY_ATTRIBUTES != 0
            || !no_extended_attributes(file)
            || !matches!(has_unknown_streams(file), Ok(false))
            || !no_explicit_security_classes(file)
        {
            return None;
        }
        security_sha256(file).ok()
    }

    fn no_extended_attributes(file: &File) -> bool {
        let mut io = IO_STATUS_BLOCK::default();
        let mut info = FILE_EA_INFORMATION { EaSize: u32::MAX };
        let status = unsafe {
            NtQueryInformationFile(
                file.as_raw_handle(),
                &mut io,
                (&mut info as *mut FILE_EA_INFORMATION).cast(),
                size_of::<FILE_EA_INFORMATION>() as u32,
                FileEaInformation,
            )
        };
        status == 0 && io.Information == size_of::<FILE_EA_INFORMATION>() && info.EaSize == 0
    }

    fn has_unknown_streams(file: &File) -> Result<bool, RootError> {
        // One successful call returns the complete stream list; a larger
        // buffer is only needed on the documented buffer errors.
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let ok = unsafe {
                GetFileInformationByHandleEx(
                    file.as_raw_handle() as HANDLE,
                    FileStreamInfo,
                    buffer.as_mut_ptr().cast(),
                    buffer.len() as u32,
                )
            };
            if ok == 0 {
                let code = unsafe { GetLastError() };
                if (code == ERROR_INSUFFICIENT_BUFFER || code == ERROR_MORE_DATA)
                    && buffer.len() < 4 * 1024 * 1024
                {
                    buffer.resize(buffer.len() * 2, 0);
                    continue;
                }
                if code == ERROR_NO_MORE_FILES || code == ERROR_HANDLE_EOF {
                    return Ok(false);
                }
                return Err(RootError::Io);
            }

            let mut offset = 0usize;
            loop {
                if offset
                    .checked_add(size_of::<FILE_STREAM_INFO>())
                    .is_none_or(|end| end > buffer.len())
                {
                    return Err(RootError::Io);
                }
                let info = unsafe {
                    ptr::read_unaligned(buffer.as_ptr().add(offset) as *const FILE_STREAM_INFO)
                };
                let name_offset = offset + offset_of!(FILE_STREAM_INFO, StreamName);
                let units = info.StreamNameLength as usize / 2;
                let name_bytes = match units.checked_mul(2) {
                    Some(bytes) => bytes,
                    None => return Err(RootError::Io),
                };
                if name_offset
                    .checked_add(name_bytes)
                    .is_none_or(|end| end > buffer.len())
                {
                    return Err(RootError::Io);
                }
                let name = unsafe {
                    std::slice::from_raw_parts(
                        buffer.as_ptr().add(name_offset).cast::<u16>(),
                        units,
                    )
                };
                if !String::from_utf16_lossy(name).eq_ignore_ascii_case("::$DATA") {
                    return Ok(true);
                }
                if info.NextEntryOffset == 0 {
                    return Ok(false);
                }
                offset = match offset.checked_add(info.NextEntryOffset as usize) {
                    Some(next) => next,
                    None => return Err(RootError::Io),
                };
            }
        }
    }

    fn security_sha256(file: &File) -> Result<[u8; 32], RootError> {
        // Observation uses READ_CONTROL, not privileged SACL access.
        let requested =
            OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
        let (descriptor, used) = query_security(file, requested)?;
        if !complete_security(descriptor.as_ptr().cast_mut().cast()) {
            return Err(RootError::Io);
        }
        let mut sha256 = [0u8; 32];
        let mut hasher = Sha256::new();
        let bytes =
            unsafe { std::slice::from_raw_parts(descriptor.as_ptr().cast::<u8>(), used as usize) };
        hasher.update(bytes);
        sha256.copy_from_slice(&hasher.finalize());
        Ok(sha256)
    }

    /// Mandatory-label, resource-attribute and scoped-policy ACEs are readable
    /// with `READ_CONTROL` even though they live in the SACL. Refuse an explicit
    /// ACE for any of those classes; an empty or absent selected result is the
    /// only ordinary case. Never request full `SACL_SECURITY_INFORMATION`.
    fn no_explicit_security_classes(file: &File) -> bool {
        [
            LABEL_SECURITY_INFORMATION,
            ATTRIBUTE_SECURITY_INFORMATION,
            SCOPE_SECURITY_INFORMATION,
        ]
        .into_iter()
        .all(|information| explicit_security_class_is_absent(file, information))
    }

    fn explicit_security_class_is_absent(file: &File, information: u32) -> bool {
        let Ok((descriptor, _)) = query_security(file, information) else {
            return false;
        };
        let mut present = 0;
        let mut sacl: *mut ACL = ptr::null_mut();
        let mut defaulted = 0;
        if unsafe {
            GetSecurityDescriptorSacl(
                descriptor.as_ptr().cast_mut().cast(),
                &mut present,
                &mut sacl,
                &mut defaulted,
            )
        } == 0
        {
            return false;
        }
        if present == 0 {
            return true;
        }
        // A present SACL must be a valid, explicitly empty ACL. A null or
        // malformed ACL is not proof that the selected class is absent.
        !sacl.is_null() && unsafe { IsValidAcl(sacl) != 0 } && unsafe { (*sacl).AceCount == 0 }
    }

    fn query_security(file: &File, requested: u32) -> Result<(Vec<u32>, u32), RootError> {
        let mut needed = 0u32;
        let ok = unsafe {
            GetKernelObjectSecurity(
                file.as_raw_handle() as HANDLE,
                requested,
                ptr::null_mut(),
                0,
                &mut needed,
            )
        };
        if ok == 0 && unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER {
            return Err(RootError::Io);
        }
        if needed == 0 {
            return Err(RootError::Io);
        }

        let mut descriptor = vec![0u32; (needed as usize).div_ceil(size_of::<u32>())];
        let capacity = needed;
        let ok = unsafe {
            GetKernelObjectSecurity(
                file.as_raw_handle() as HANDLE,
                requested,
                descriptor.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        };
        if ok == 0 || needed > capacity {
            return Err(RootError::Io);
        }
        Ok((descriptor, needed))
    }

    fn complete_security(descriptor: *mut std::ffi::c_void) -> bool {
        let mut owner = ptr::null_mut();
        let mut group = ptr::null_mut();
        let mut dacl: *mut ACL = ptr::null_mut();
        let mut present = 0;
        let mut defaulted = 0;
        unsafe {
            GetSecurityDescriptorOwner(descriptor, &mut owner, &mut defaulted) != 0
                && !owner.is_null()
                && IsValidSid(owner) != 0
                && GetSecurityDescriptorGroup(descriptor, &mut group, &mut defaulted) != 0
                && !group.is_null()
                && IsValidSid(group) != 0
                && GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted)
                    != 0
                && present != 0
                && !dacl.is_null()
                && IsValidAcl(dacl) != 0
        }
    }

    fn is_sharing(code: u32) -> bool {
        matches!(
            code,
            ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION | ERROR_ACCESS_DENIED
        )
    }

    /// Fingerprint an already open regular file. The caller owns the boundary
    /// decision; this helper only proves the object's own shape, metadata,
    /// bytes and stability across the read.
    pub(crate) fn fingerprint_open_file(file: &File) -> Result<Fingerprint, RootError> {
        let basic = basic_info(file)?;
        let standard = standard_info(file)?;
        if is_reparse(&attribute_tag(file)?) || standard.Directory {
            return Err(RootError::UnsupportedTarget(UnsupportedReason::ObjectKind));
        }
        let identity = identity(file)?;
        let Some(security_sha256) = eligible_metadata(file, &basic, &standard) else {
            return Err(RootError::UnsupportedTarget(UnsupportedReason::Metadata));
        };

        let mut hasher = Sha256::new();
        let mut reader = file;
        let mut buffer = [0u8; 64 * 1024];
        let mut len = 0u64;
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    hasher.update(&buffer[..read]);
                    len += read as u64;
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(RootError::Io),
            }
        }
        check_original(file, &identity, &basic, &standard)?;
        if len != standard.EndOfFile as u64
            || eligible_metadata(file, &basic, &standard) != Some(security_sha256)
        {
            return Err(RootError::Io);
        }
        let mut sha256 = [0u8; 32];
        sha256.copy_from_slice(&hasher.finalize());
        Ok(Fingerprint {
            identity,
            len,
            sha256,
            metadata: FileMetadata::Windows {
                attributes: basic.FileAttributes,
                last_write_100ns: basic.LastWriteTime,
                links: standard.NumberOfLinks,
                security_sha256,
            },
        })
    }

    /// Fingerprint an operation-owned artifact outside the installation tree.
    /// Synthetic proof only; the caller is responsible for its own binding.
    pub(crate) fn fingerprint_path(path: &Path) -> Result<Fingerprint, RootError> {
        let text = path.to_str().ok_or(RootError::TargetUnavailable)?;
        let file = open_path(text, GENERIC_READ).map_err(|code| {
            if matches!(code, ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND) {
                RootError::TargetUnavailable
            } else {
                RootError::Io
            }
        })?;
        fingerprint_open_file(&file)
    }

    pub(crate) struct PrivateFile {
        _file: File,
        name: String,
        volume: u64,
        file_id: [u8; 16],
    }

    impl PrivateFile {
        pub(crate) fn path(&self, parent: &File) -> Result<PathBuf, RootError> {
            Ok(PathBuf::from(join(&final_path(parent)?, &self.name)))
        }

        pub(crate) fn verify(&self, parent: &File) -> Result<(), RootError> {
            let expected = join(&final_path(parent)?, &self.name);
            let file = open_pin_path(&expected).map_err(|_| RootError::TargetUnavailable)?;
            let tag = attribute_tag(&file)?;
            let standard = standard_info(&file)?;
            let id = file_id(&file)?;
            let parent_id = file_id(parent)?;
            if is_reparse(&tag)
                || standard.Directory
                || id.VolumeSerialNumber != self.volume
                || id.FileId.Identifier != self.file_id
                || id.VolumeSerialNumber != parent_id.VolumeSerialNumber
                || !final_path(&file)?.eq_ignore_ascii_case(&expected)
            {
                return Err(RootError::TargetUnavailable);
            }
            Ok(())
        }
    }

    pub(crate) fn open_or_create_private_file(
        parent: &File,
        name: &str,
    ) -> Result<PrivateFile, RootError> {
        if !ordinary_component(name) {
            return Err(RootError::TargetUnavailable);
        }
        let path = join(&final_path(parent)?, name);
        let file = match open_pin_path(&path) {
            Ok(file) => file,
            Err(ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND) => create_pin_path(&path)?,
            Err(_) => return Err(RootError::TargetUnavailable),
        };
        let tag = attribute_tag(&file)?;
        let standard = standard_info(&file)?;
        let id = file_id(&file)?;
        let parent_id = file_id(parent)?;
        if is_reparse(&tag)
            || standard.Directory
            || id.VolumeSerialNumber == 0
            || id.VolumeSerialNumber != parent_id.VolumeSerialNumber
            || !final_path(&file)?.eq_ignore_ascii_case(&path)
        {
            return Err(RootError::TargetUnavailable);
        }
        Ok(PrivateFile {
            _file: file,
            name: name.to_owned(),
            volume: id.VolumeSerialNumber,
            file_id: id.FileId.Identifier,
        })
    }

    fn open_pin_path(path: &str) -> Result<File, u32> {
        let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                ptr::null(),
                OPEN_EXISTING,
                NO_FOLLOW,
                ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(unsafe { GetLastError() });
        }
        Ok(unsafe { File::from_raw_handle(handle) })
    }

    fn create_pin_path(path: &str) -> Result<File, RootError> {
        let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                ptr::null(),
                CREATE_NEW,
                FILE_ATTRIBUTE_NORMAL | NO_FOLLOW,
                ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(RootError::Io);
        }
        Ok(unsafe { File::from_raw_handle(handle) })
    }

    pub(crate) fn open_private_directory(path: &Path) -> Result<File, RootError> {
        open_selection(path, true).map(|mut chain| chain.pop().expect("pinned root").file)
    }

    pub(crate) fn open_private_child_directory(
        parent: &File,
        name: &str,
    ) -> Result<File, RootError> {
        if !ordinary_component(name) {
            return Err(RootError::TargetUnavailable);
        }
        open_private_directory(Path::new(&join(&final_path(parent)?, name)))
    }

    pub(crate) fn private_child_path(parent: &File, name: &str) -> Result<PathBuf, RootError> {
        if !ordinary_component(name) {
            return Err(RootError::TargetUnavailable);
        }
        Ok(PathBuf::from(join(&final_path(parent)?, name)))
    }

    pub(crate) fn verify_private_directory(path: &Path, handle: &File) -> Result<(), RootError> {
        let expected = directory_identity(path)?;
        let actual = RootIdentity::from_adapter_evidence(identity(handle)?);
        if expected != actual {
            return Err(RootError::TargetUnavailable);
        }
        Ok(())
    }

    pub(crate) fn directory_identity(path: &Path) -> Result<RootIdentity, RootError> {
        let text = path.to_str().ok_or(RootError::TargetUnavailable)?;
        let file = open_path(text, DIRECTORY_ACCESS).map_err(|_| RootError::TargetUnavailable)?;
        Ok(verify_root(file)?.identity)
    }
}

pub(crate) use root::{
    Binding, PrivateFile, ResolvedTarget, bind_installation, directory_identity, fingerprint_path,
    observe_target, open_or_create_private_file, open_private_child_directory,
    open_private_directory, private_child_path, reopen_installation, resolve_existing_parent,
    verify_private_directory,
};
