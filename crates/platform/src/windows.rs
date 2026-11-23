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
use marduk_core::qualification::{Capability, Os, Route, VolumeClass, available};
use marduk_core::root_identity::RootError;

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
// handles share read and write but deny delete sharing from the volume root
// through the resolved parent, so the bound name cannot be renamed or replaced
// while writes inside it remain possible. Observation is not write authority.
//
// A shared write, not exclusive access, is what the product needs: the pinned
// app root must coexist with the native stage that creates inside it, and a
// pinned target parent must accept the kernel's own open for a move
// destination. Delete sharing stays denied; that is the name-binding
// mechanism.
// ---------------------------------------------------------------------------
mod root {
    use super::ERROR_ALREADY_EXISTS;
    use std::fs::File;
    use std::io;
    use std::io::Read;
    use std::mem::offset_of;
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use std::path::{Component, Path, PathBuf, Prefix};
    use std::ptr;
    use std::sync::Mutex;

    use marduk_core::qualification::{Capability, Os, Route, VolumeClass, available};
    use marduk_core::root_identity::{
        FileMetadata, Fingerprint, GameLayout, NativeIdentity, RootError, RootIdentity,
        TargetObservation, UnsupportedReason,
    };
    use marduk_core::targets::RelativePath;
    use sha2::{Digest, Sha256};
    use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
    use windows_sys::Wdk::Storage::FileSystem::{
        FILE_CREATE, FILE_DIRECTORY_FILE, FILE_EA_INFORMATION, FILE_NON_DIRECTORY_FILE, FILE_OPEN,
        FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT, FileEaInformation, NtCreateFile,
        NtQueryInformationFile,
    };
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_DIR_NOT_EMPTY, ERROR_FILE_EXISTS,
        ERROR_FILE_NOT_FOUND, ERROR_HANDLE_EOF, ERROR_INSUFFICIENT_BUFFER, ERROR_INVALID_NAME,
        ERROR_LOCK_VIOLATION, ERROR_MORE_DATA, ERROR_NO_MORE_FILES, ERROR_PATH_NOT_FOUND,
        ERROR_SHARING_VIOLATION, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE,
        INVALID_HANDLE_VALUE, OBJ_CASE_INSENSITIVE, OBJ_DONT_REPARSE, RtlNtStatusToDosError,
        UNICODE_STRING,
    };
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_REVISION, ACL_SIZE_INFORMATION,
        ATTRIBUTE_SECURITY_INFORMATION, AclSizeInformation, AddAccessAllowedAceEx,
        CONTAINER_INHERIT_ACE, CopySid, DACL_SECURITY_INFORMATION, EqualSid,
        GROUP_SECURITY_INFORMATION, GetAce, GetAclInformation, GetKernelObjectSecurity,
        GetLengthSid, GetSecurityDescriptorControl, GetSecurityDescriptorDacl,
        GetSecurityDescriptorGroup, GetSecurityDescriptorOwner, GetSecurityDescriptorSacl,
        GetTokenInformation, INHERIT_ONLY_ACE, INHERITED_ACE, InitializeAcl,
        InitializeSecurityDescriptor, IsValidAcl, IsValidSid, LABEL_SECURITY_INFORMATION,
        MakeSelfRelativeSD, NO_PROPAGATE_INHERIT_ACE, OBJECT_INHERIT_ACE,
        OWNER_SECURITY_INFORMATION, PSID, SCOPE_SECURITY_INFORMATION, SE_DACL_PROTECTED,
        SECURITY_DESCRIPTOR, SetSecurityDescriptorControl, SetSecurityDescriptorDacl,
        SetSecurityDescriptorOwner, TOKEN_OWNER, TOKEN_QUERY, TokenOwner,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CREATE_NEW, CreateDirectoryW, CreateFileW, DELETE, FILE_ALL_ACCESS, FILE_ATTRIBUTE_ARCHIVE,
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_NORMAL,
        FILE_ATTRIBUTE_NOT_CONTENT_INDEXED, FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_ATTRIBUTE_SYSTEM, FILE_ATTRIBUTE_TAG_INFO, FILE_BASIC_INFO, FILE_DISPOSITION_INFO,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_EXTD_DIR_INFO,
        FILE_ID_INFO, FILE_INFO_BY_HANDLE_CLASS, FILE_LIST_DIRECTORY, FILE_NAME_NORMALIZED,
        FILE_READ_ATTRIBUTES, FILE_READ_EA, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        FILE_STANDARD_INFO, FILE_STREAM_INFO, FILE_TRAVERSE, FileAttributeTagInfo, FileBasicInfo,
        FileDispositionInfo, FileIdExtdDirectoryInfo, FileIdExtdDirectoryRestartInfo, FileIdInfo,
        FileStandardInfo, FileStreamInfo, FindClose, FindFirstFileW, GetFileInformationByHandleEx,
        GetFinalPathNameByHandleW, GetVolumeInformationByHandleW,
        GetVolumeNameForVolumeMountPointW, OPEN_EXISTING, READ_CONTROL, ReOpenFile, SYNCHRONIZE,
        SetFileInformationByHandle, VOLUME_NAME_GUID, WIN32_FIND_DATAW,
    };
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    const NO_FOLLOW: u32 = FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT;
    /// Pinned directories share write so the process can create and replace
    /// entries inside them, but never delete sharing: while the pin lives the
    /// bound name cannot be renamed or replaced.
    const PIN_SHARE: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE;
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

        /// The target pathname rebuilt from the verified parent's final path
        /// and the resolved leaf. The parent handle owns the spelling; the
        /// caller's or archive's spelling is not used here.
        pub(crate) fn public_path(&self) -> PathBuf {
            PathBuf::from(join(&self.parent().final_path, &self.leaf))
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
        open_selection_shared(path, create, PIN_SHARE, PIN_SHARE)
    }

    fn open_selection_shared(
        path: &Path,
        create: bool,
        ancestor_share: u32,
        final_share: u32,
    ) -> Result<Vec<RootDir>, RootError> {
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
        let mut parts = parts.peekable();
        while let Some(part) = parts.next() {
            let Component::Normal(name) = part else {
                return Err(RootError::TargetUnavailable);
            };
            let name = name.to_str().ok_or(RootError::TargetUnavailable)?;
            let parent = chain.last().expect("volume root is pinned");
            let share = if parts.peek().is_none() {
                final_share
            } else {
                ancestor_share
            };
            match pin_selected_directory_shared(parent, name, share) {
                Ok(child) => chain.push(child),
                Err(RootError::TargetUnavailable) if create => {
                    if leaf_match(parent, name)?.is_some() {
                        return Err(RootError::TargetUnavailable);
                    }
                    create_directory(parent, name)?;
                    chain.push(pin_selected_directory_shared(parent, name, share)?);
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
        open_path_shared(path, access, FILE_SHARE_READ)
    }

    fn open_path_shared(path: &str, access: u32, share: u32) -> Result<File, u32> {
        let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                access,
                share,
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
        open_entry_shared(parent, entry, access, FILE_SHARE_READ)
    }

    fn open_entry_shared(
        parent: &RootDir,
        entry: &DirectoryEntry,
        access: u32,
        share: u32,
    ) -> Result<File, RootError> {
        check_locator(parent)?;
        let file = open_path_shared(&join(&parent.final_path, &entry.name), access, share)
            .map_err(|code| {
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
        let file = open_entry_shared(parent, &entry, DIRECTORY_ACCESS, PIN_SHARE)?;
        let child = verify_root(file)?;
        if child.final_path != join(&parent.final_path, &entry.name)
            || child.volume != parent.volume
            || child.identity == parent.identity
        {
            return Err(RootError::OutsideBoundary);
        }
        Ok(child)
    }

    fn pin_selected_directory_shared(
        parent: &RootDir,
        name: &str,
        share: u32,
    ) -> Result<RootDir, RootError> {
        if !ordinary_component(name) {
            return Err(RootError::TargetUnavailable);
        }
        check_locator(parent).map_err(|_| RootError::TargetUnavailable)?;
        // Canonicalize one component at a time. A single open of the whole
        // selection with OPEN_REPARSE_POINT only protects its final component.
        let file = open_path_shared(&join(&parent.final_path, name), DIRECTORY_ACCESS, share)
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
        filesystem_name(file).map(|_| ())
    }

    /// File-system name reported for the volume that owns this handle.
    fn filesystem_name(file: &File) -> Result<String, RootError> {
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
        String::from_utf16(&name[..end]).map_err(|_| RootError::Io)
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

    /// Digest what a descriptor grants: owner SID, group SID and the DACL as an
    /// ordered list of (ACE type, inheritance flags, mask, object GUIDs, SID).
    /// Self-relative layout, padding and control/provenance bits are ignored.
    /// ACE order is preserved because Windows evaluates entries in order, so
    /// reordering overlapping allow/deny entries can change effective access;
    /// two descriptors with the same entries in a different order therefore
    /// hash differently. Observation uses `READ_CONTROL`, not privileged SACL
    /// access.
    fn security_sha256(file: &File) -> Result<[u8; 32], RootError> {
        let requested =
            OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
        let (descriptor, _) = query_security(file, requested)?;
        security_digest(descriptor.as_ptr().cast_mut().cast())
    }

    fn security_digest(descriptor: *mut std::ffi::c_void) -> Result<[u8; 32], RootError> {
        let mut owner: PSID = ptr::null_mut();
        let mut group: PSID = ptr::null_mut();
        let mut present = 0;
        let mut acl: *mut ACL = ptr::null_mut();
        let mut defaulted = 0;
        if unsafe {
            GetSecurityDescriptorOwner(descriptor, &mut owner, &mut defaulted) == 0
                || GetSecurityDescriptorGroup(descriptor, &mut group, &mut defaulted) == 0
                || GetSecurityDescriptorDacl(descriptor, &mut present, &mut acl, &mut defaulted)
                    == 0
                || present == 0
                || acl.is_null()
                || IsValidAcl(acl) == 0
        } {
            return Err(RootError::Io);
        }
        let owner = canonical_sid(owner)?;
        let group = canonical_sid(group)?;

        let mut information = ACL_SIZE_INFORMATION::default();
        if unsafe {
            GetAclInformation(
                acl,
                (&mut information as *mut ACL_SIZE_INFORMATION).cast(),
                size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            )
        } == 0
        {
            return Err(RootError::Io);
        }
        let mut entries = Vec::with_capacity(information.AceCount as usize);
        for index in 0..information.AceCount {
            let mut ace: *mut std::ffi::c_void = ptr::null_mut();
            if unsafe { GetAce(acl, u32::from(index), &mut ace) } == 0 || ace.is_null() {
                return Err(RootError::Io);
            }
            entries.push(canonical_ace(ace)?);
        }

        let mut hasher = Sha256::new();
        hasher.update(b"windows-security-v1\0");
        for chunk in [&owner, &group] {
            hasher.update((chunk.len() as u32).to_le_bytes());
            hasher.update(chunk);
        }
        hasher.update((entries.len() as u32).to_le_bytes());
        for entry in &entries {
            hasher.update((entry.len() as u32).to_le_bytes());
            hasher.update(entry);
        }
        let mut sha256 = [0u8; 32];
        sha256.copy_from_slice(&hasher.finalize());
        Ok(sha256)
    }

    fn canonical_sid(sid: PSID) -> Result<Vec<u8>, RootError> {
        if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
            return Err(RootError::Io);
        }
        let length = unsafe { GetLengthSid(sid) } as usize;
        if length == 0 {
            return Err(RootError::Io);
        }
        Ok(unsafe { std::slice::from_raw_parts(sid.cast::<u8>(), length) }.to_vec())
    }

    /// One DACL entry: type, effective inheritance flags, mask, object GUIDs
    /// and SID. `INHERITED_ACE` and the audit-only flag bits record provenance,
    /// not access, and are dropped. ACE classes this parser cannot read fail
    /// closed instead of being hashed at face value.
    fn canonical_ace(ace: *mut std::ffi::c_void) -> Result<Vec<u8>, RootError> {
        const ACCESS_DENIED_ACE_TYPE: u8 = 1;
        const ACCESS_ALLOWED_OBJECT_ACE_TYPE: u8 = 5;
        const ACCESS_DENIED_OBJECT_ACE_TYPE: u8 = 6;
        const ACE_OBJECT_TYPE_PRESENT: u32 = 1;
        const ACE_INHERITED_OBJECT_TYPE_PRESENT: u32 = 2;
        const ACE_INHERITANCE_FLAGS: u8 = (OBJECT_INHERIT_ACE
            | CONTAINER_INHERIT_ACE
            | NO_PROPAGATE_INHERIT_ACE
            | INHERIT_ONLY_ACE) as u8;

        let header = unsafe { ptr::read(ace.cast::<ACE_HEADER>()) };
        let size = header.AceSize as usize;
        if size < size_of::<ACE_HEADER>() {
            return Err(RootError::Io);
        }
        let raw = unsafe { std::slice::from_raw_parts(ace.cast::<u8>(), size) };
        let read_u32 = |offset: usize| -> Result<u32, RootError> {
            raw.get(offset..offset + 4)
                .map(|bytes| u32::from_le_bytes(bytes.try_into().expect("four bytes")))
                .ok_or(RootError::Io)
        };
        let mask = read_u32(4)?;

        let sid_offset = match header.AceType {
            ACCESS_ALLOWED_ACE_TYPE | ACCESS_DENIED_ACE_TYPE => 8,
            ACCESS_ALLOWED_OBJECT_ACE_TYPE | ACCESS_DENIED_OBJECT_ACE_TYPE => {
                let flags = read_u32(8)?;
                let mut offset = 12;
                if flags & ACE_OBJECT_TYPE_PRESENT != 0 {
                    offset += 16;
                }
                if flags & ACE_INHERITED_OBJECT_TYPE_PRESENT != 0 {
                    offset += 16;
                }
                offset
            }
            _ => return Err(RootError::Io),
        };
        if sid_offset + 8 > size {
            return Err(RootError::Io);
        }
        let sid: PSID = unsafe { ace.cast::<u8>().add(sid_offset).cast() };
        if unsafe { IsValidSid(sid) } == 0 {
            return Err(RootError::Io);
        }
        let sid_length = unsafe { GetLengthSid(sid) } as usize;
        if sid_length == 0 || sid_offset + sid_length > size {
            return Err(RootError::Io);
        }

        let object_fields = &raw[8..sid_offset];
        let mut entry = Vec::with_capacity(9 + object_fields.len() + sid_length);
        entry.push(header.AceType);
        entry.push(header.AceFlags & ACE_INHERITANCE_FLAGS);
        entry.extend_from_slice(&mask.to_le_bytes());
        entry.extend_from_slice(&(object_fields.len() as u8).to_le_bytes());
        entry.extend_from_slice(object_fields);
        entry.extend_from_slice(&(sid_length as u32).to_le_bytes());
        entry.extend_from_slice(&raw[sid_offset..sid_offset + sid_length]);
        Ok(entry)
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
            // The handle held for the catalog's lifetime is the authority: the
            // pathname is resolved afresh here, so a replaced leaf could pass a
            // fresh-open check while the held object is a different file.
            let pinned = file_id(&self._file)?;
            if is_reparse(&tag)
                || standard.Directory
                || id.VolumeSerialNumber != self.volume
                || id.FileId.Identifier != self.file_id
                || id.VolumeSerialNumber != parent_id.VolumeSerialNumber
                || pinned.VolumeSerialNumber != self.volume
                || pinned.FileId.Identifier != self.file_id
                || pinned.VolumeSerialNumber != parent_id.VolumeSerialNumber
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
                // No delete sharing: while this handle lives, the operating
                // system refuses any rename or replace of the catalog leaf.
                FILE_SHARE_READ | FILE_SHARE_WRITE,
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
                FILE_SHARE_READ | FILE_SHARE_WRITE,
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
        // This proof may run beside a writable pin on the same directory (the
        // native stage or a target parent), so it must also share write while
        // still denying delete.
        let file = open_path_shared(text, DIRECTORY_ACCESS, PIN_SHARE)
            .map_err(|_| RootError::TargetUnavailable)?;
        Ok(verify_root(file)?.identity)
    }

    // -----------------------------------------------------------------------
    // Verified private staging tree.
    //
    // The library root, `revisions/` and every bound revision are opened as
    // no-follow directory handles that deny delete sharing, so the operating
    // system refuses to rename or replace them while a stage is bound. Every
    // child open below uses `NtCreateFile` with
    // `OBJECT_ATTRIBUTES.RootDirectory` set to one of those handles. Revision
    // directories are created with a protected owner-only DACL (the Windows
    // analogue of mode `0700`); identity is re-proved from the live handle on
    // every re-observe. The residual and the durability contract are owned by
    // `docs/contracts/m1-recovery.md`.
    // -----------------------------------------------------------------------

    const STAGE_DIRECTORY_ACCESS: u32 =
        DIRECTORY_ACCESS | FILE_TRAVERSE | GENERIC_WRITE | READ_CONTROL | SYNCHRONIZE;
    const STAGE_FILE_ACCESS: u32 = GENERIC_READ | GENERIC_WRITE | SYNCHRONIZE;
    const STAGE_SHARE: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE;
    const STAGE_DELETE_SHARE: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    const STAGE_DIRECTORY_OPTIONS: u32 =
        FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT;
    const STAGE_FILE_OPTIONS: u32 =
        FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT;
    const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
    const SECURITY_DESCRIPTOR_REVISION: u32 = 1;

    pub(crate) struct StagedDirectory {
        root: RootDir,
        private: bool,
    }

    pub(crate) struct StagingTree {
        root: RootDir,
        root_path: PathBuf,
        revisions: StagedDirectory,
    }

    impl StagedDirectory {
        pub(crate) fn identity(&self) -> &RootIdentity {
            &self.root.identity
        }

        fn from_verified(root: &RootDir, file: File, private: bool) -> Result<Self, RootError> {
            let directory = verify_root(file)?;
            if directory.volume != root.volume || directory.identity == root.identity {
                return Err(RootError::OutsideBoundary);
            }
            let staged = Self {
                root: directory,
                private,
            };
            staged.verify()?;
            Ok(staged)
        }

        fn verify(&self) -> Result<(), RootError> {
            check_locator(&self.root)?;
            verify_staged_directory(&self.root.file, self.root.volume)?;
            if !owned_by_current_user(&self.root.file)? {
                return Err(RootError::TargetUnavailable);
            }
            if self.private && !private_dacl(&self.root.file)? {
                return Err(RootError::TargetUnavailable);
            }
            Ok(())
        }

        fn open_dir_at(&self, relative: &[&str]) -> io::Result<File> {
            let mut current = self.root.file.try_clone()?;
            for component in checked_components(relative)? {
                current = nt_open_relative(
                    &current,
                    component,
                    STAGE_DIRECTORY_ACCESS,
                    STAGE_SHARE,
                    FILE_OPEN,
                    STAGE_DIRECTORY_OPTIONS,
                    0,
                    None,
                )
                .map_err(nt_io)?;
                verify_staged_directory(&current, self.root.volume).map_err(root_io)?;
            }
            Ok(current)
        }

        /// Create a missing private directory component relative to the held
        /// handle. An existing component is reopened without following a
        /// reparse point and its owner is checked, so a pre-created substitute
        /// cannot redirect later writes.
        fn ensure_dir_at(&self, relative: &[&str]) -> io::Result<File> {
            let mut current = self.root.file.try_clone()?;
            for component in checked_components(relative)? {
                let next = match nt_open_relative(
                    &current,
                    component,
                    STAGE_DIRECTORY_ACCESS,
                    STAGE_SHARE,
                    FILE_OPEN,
                    STAGE_DIRECTORY_OPTIONS,
                    0,
                    None,
                ) {
                    Ok(file) => file,
                    Err(code) if matches!(code, ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND) => {
                        let security = private_security_descriptor(true).map_err(root_io)?;
                        match nt_open_relative(
                            &current,
                            component,
                            STAGE_DIRECTORY_ACCESS,
                            STAGE_SHARE,
                            FILE_CREATE,
                            STAGE_DIRECTORY_OPTIONS,
                            0,
                            Some(&security),
                        ) {
                            Ok(file) => file,
                            Err(ERROR_FILE_EXISTS | ERROR_ALREADY_EXISTS) => nt_open_relative(
                                &current,
                                component,
                                STAGE_DIRECTORY_ACCESS,
                                STAGE_SHARE,
                                FILE_OPEN,
                                STAGE_DIRECTORY_OPTIONS,
                                0,
                                None,
                            )
                            .map_err(nt_io)?,
                            Err(code) => return Err(nt_io(code)),
                        }
                    }
                    Err(code) => return Err(nt_io(code)),
                };
                verify_staged_directory(&next, self.root.volume).map_err(root_io)?;
                if !owned_by_current_user(&next).map_err(root_io)? {
                    return Err(io::Error::from_raw_os_error(ERROR_ACCESS_DENIED as i32));
                }
                current = next;
            }
            Ok(current)
        }

        pub(crate) fn create_private_file(&self, relative: &[&str]) -> io::Result<File> {
            let (leaf, parents) = relative
                .split_last()
                .ok_or_else(|| io::Error::from_raw_os_error(ERROR_INVALID_NAME as i32))?;
            let directory = self.ensure_dir_at(parents)?;
            let security = private_security_descriptor(false).map_err(root_io)?;
            let file = nt_open_relative(
                &directory,
                leaf,
                STAGE_FILE_ACCESS,
                STAGE_SHARE,
                FILE_CREATE,
                STAGE_FILE_OPTIONS,
                FILE_ATTRIBUTE_NORMAL,
                Some(&security),
            )
            .map_err(nt_io)?;
            verify_staged_file(&file, self.root.volume).map_err(root_io)?;
            Ok(file)
        }

        pub(crate) fn open_relative_file(&self, relative: &[&str]) -> io::Result<File> {
            let (leaf, parents) = relative
                .split_last()
                .ok_or_else(|| io::Error::from_raw_os_error(ERROR_INVALID_NAME as i32))?;
            let directory = self.open_dir_at(parents)?;
            let file = nt_open_relative(
                &directory,
                leaf,
                STAGE_FILE_ACCESS,
                STAGE_SHARE,
                FILE_OPEN,
                STAGE_FILE_OPTIONS,
                0,
                None,
            )
            .map_err(nt_io)?;
            verify_staged_file(&file, self.root.volume).map_err(root_io)?;
            Ok(file)
        }

        /// List one directory relative to the held handle. The returned booleans
        /// are `is_file` and `is_dir`; a reparse point or third object kind is
        /// reported as neither.
        pub(crate) fn list_relative(
            &self,
            relative: &[&str],
        ) -> io::Result<Vec<(String, bool, bool)>> {
            let directory = self.open_dir_at(relative)?;
            let entries = read_directory(&directory).map_err(root_io)?;
            Ok(entries
                .into_iter()
                .map(|entry| {
                    let reparse = entry.is_reparse();
                    let is_directory = entry.attributes & FILE_ATTRIBUTE_DIRECTORY != 0;
                    (
                        entry.name,
                        !reparse && !is_directory,
                        !reparse && is_directory,
                    )
                })
                .collect())
        }

        pub(crate) fn remove_relative_file(&self, relative: &[&str]) -> io::Result<()> {
            let (leaf, parents) = relative
                .split_last()
                .ok_or_else(|| io::Error::from_raw_os_error(ERROR_INVALID_NAME as i32))?;
            let directory = self.open_dir_at(parents)?;
            let file = match nt_open_relative(
                &directory,
                leaf,
                DELETE | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
                STAGE_DELETE_SHARE,
                FILE_OPEN,
                STAGE_FILE_OPTIONS,
                0,
                None,
            ) {
                Ok(file) => file,
                Err(ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND) => return Ok(()),
                Err(code) => return Err(nt_io(code)),
            };
            verify_staged_file(&file, self.root.volume).map_err(root_io)?;
            dispose(file)
        }

        /// Remove an empty directory relative to the held handle. `Ok(false)`
        /// means the directory still had entries; `Ok(true)` means it was
        /// removed or was already absent.
        pub(crate) fn remove_relative_dir(&self, relative: &[&str]) -> io::Result<bool> {
            let (leaf, parents) = relative
                .split_last()
                .ok_or_else(|| io::Error::from_raw_os_error(ERROR_INVALID_NAME as i32))?;
            let directory = self.open_dir_at(parents)?;
            let target = match nt_open_relative(
                &directory,
                leaf,
                DELETE | STAGE_DIRECTORY_ACCESS,
                STAGE_DELETE_SHARE,
                FILE_OPEN,
                STAGE_DIRECTORY_OPTIONS,
                0,
                None,
            ) {
                Ok(target) => target,
                Err(ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND) => return Ok(true),
                Err(code) => return Err(nt_io(code)),
            };
            verify_staged_directory(&target, self.root.volume).map_err(root_io)?;
            if !read_directory(&target).map_err(root_io)?.is_empty() {
                return Ok(false);
            }
            match dispose(target) {
                Ok(()) => Ok(true),
                Err(error) => match error.raw_os_error() {
                    Some(code) if code as u32 == ERROR_DIR_NOT_EMPTY => Ok(false),
                    Some(code) if code as u32 == ERROR_FILE_NOT_FOUND => Ok(true),
                    _ => Err(error),
                },
            }
        }

        pub(crate) fn sync_relative_dir(&self, relative: &[&str]) -> io::Result<()> {
            self.open_dir_at(relative)?.sync_all()
        }
    }

    impl StagingTree {
        pub(crate) fn open(root: &Path) -> Result<Self, RootError> {
            let text = root.to_str().ok_or(RootError::TargetUnavailable)?;
            // No delete sharing: while this handle is bound the root name cannot
            // be renamed or replaced. `sync_root` flushes this handle; this open
            // does not flush it.
            let file = open_shared_path(text, STAGE_DIRECTORY_ACCESS)
                .map_err(|_| RootError::TargetUnavailable)?;
            let root_directory = verify_root(file)?;
            if !filesystem_name(&root_directory.file)?.eq_ignore_ascii_case("NTFS") {
                return Err(RootError::UnsupportedTarget(
                    UnsupportedReason::Configuration,
                ));
            }
            if !owned_by_current_user(&root_directory.file)? {
                return Err(RootError::OutsideBoundary);
            }
            let revisions_file = nt_open_relative(
                &root_directory.file,
                "revisions",
                STAGE_DIRECTORY_ACCESS,
                STAGE_SHARE,
                FILE_OPEN,
                STAGE_DIRECTORY_OPTIONS,
                0,
                None,
            )
            .map_err(open_child_error)?;
            // Product creation binds `revisions/` through
            // `open_private_child_directory` under a private root, so this open
            // does not re-check the DACL: the privacy boundary is creation, and
            // a re-check here would only be defence in depth for pre-created
            // trees.
            let revisions = StagedDirectory::from_verified(&root_directory, revisions_file, false)?;
            let root_path = std::path::absolute(root).map_err(|_| RootError::TargetUnavailable)?;
            let tree = Self {
                root: root_directory,
                root_path,
                revisions,
            };
            tree.reverify_revisions()?;
            Ok(tree)
        }

        pub(crate) fn reverify_revisions(&self) -> Result<(), RootError> {
            self.revisions.verify()?;
            check_locator(&self.root)?;
            let current = verify_root(open_existing_directory(&self.root_path.join("revisions"))?)?;
            if current.identity != self.revisions.root.identity {
                return Err(RootError::TargetUnavailable);
            }
            Ok(())
        }

        pub(crate) fn create_revision(&self, name: &str) -> Result<StagedDirectory, RootError> {
            self.reverify_revisions()?;
            let security = private_security_descriptor(true)?;
            let file = nt_open_relative(
                &self.revisions.root.file,
                name,
                STAGE_DIRECTORY_ACCESS,
                STAGE_SHARE,
                FILE_CREATE,
                STAGE_DIRECTORY_OPTIONS,
                0,
                Some(&security),
            )
            .map_err(open_child_error)?;
            let revision = StagedDirectory::from_verified(&self.root, file, true)?;
            self.verify_revision_path(name, &revision)?;
            Ok(revision)
        }

        pub(crate) fn open_revision(
            &self,
            name: &str,
        ) -> Result<Option<StagedDirectory>, RootError> {
            let file = match nt_open_relative(
                &self.revisions.root.file,
                name,
                STAGE_DIRECTORY_ACCESS,
                STAGE_SHARE,
                FILE_OPEN,
                STAGE_DIRECTORY_OPTIONS,
                0,
                None,
            ) {
                Ok(file) => file,
                Err(ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND) => return Ok(None),
                Err(code) => return Err(open_child_error(code)),
            };
            StagedDirectory::from_verified(&self.root, file, true).map(Some)
        }

        /// Reopen the exact revision relative to the verified handle and prove
        /// the path still names the same object.
        pub(crate) fn reobserve(&self, name: &str) -> Result<StagedDirectory, RootError> {
            self.reverify_revisions()?;
            let revision = self
                .open_revision(name)?
                .ok_or(RootError::TargetUnavailable)?;
            self.verify_revision_path(name, &revision)?;
            Ok(revision)
        }

        pub(crate) fn reverify_revision(
            &self,
            name: &str,
            revision: &StagedDirectory,
        ) -> Result<(), RootError> {
            revision.verify()?;
            self.reverify_revisions()?;
            self.verify_revision_path(name, revision)
        }

        pub(crate) fn sync_revisions(&self) -> Result<(), RootError> {
            self.revisions
                .root
                .file
                .sync_all()
                .map_err(|_| RootError::Io)
        }

        /// Flush the bound root handle. Reclaim uses this instead of reopening
        /// the root pathname, which the no-delete pin would refuse.
        pub(crate) fn sync_root(&self) -> Result<(), RootError> {
            self.root.file.sync_all().map_err(|_| RootError::Io)
        }

        pub(crate) fn remove_revision_dir(&self, name: &str) -> io::Result<bool> {
            self.revisions.remove_relative_dir(&[name])
        }

        fn verify_revision_path(
            &self,
            name: &str,
            revision: &StagedDirectory,
        ) -> Result<(), RootError> {
            if !stage_component(name) {
                return Err(RootError::TargetUnavailable);
            }
            let current = verify_root(open_existing_directory(
                &self.root_path.join("revisions").join(name),
            )?)?;
            if current.identity != revision.root.identity {
                return Err(RootError::TargetUnavailable);
            }
            Ok(())
        }
    }

    /// Volume class of the opened root, read from the volume that owns the
    /// handle, never from the path spelling; a reparse final component is
    /// refused. The adapter reports the class only:
    /// `marduk_core::qualification` decides what it is good for.
    pub(crate) fn volume_class(root: &Path) -> VolumeClass {
        let Some(text) = root.to_str() else {
            return VolumeClass::Other;
        };
        let Ok(file) = open_existing_directory(Path::new(text)) else {
            return VolumeClass::Other;
        };
        let Ok(tag) = attribute_tag(&file) else {
            return VolumeClass::Other;
        };
        let Ok(standard) = standard_info(&file) else {
            return VolumeClass::Other;
        };
        if is_reparse(&tag) || !standard.Directory || identity(&file).is_err() {
            return VolumeClass::Other;
        }
        match filesystem_name(&file) {
            Ok(name) if name.eq_ignore_ascii_case("NTFS") => VolumeClass::Ntfs,
            _ => VolumeClass::Other,
        }
    }

    /// Whether the private staging adapter may run on this volume. The record
    /// proves the staging layer on NTFS; the import gate is a higher rung and
    /// stays closed.
    pub(crate) fn staging_volume_qualified(root: &Path) -> bool {
        available(
            Capability::LibraryStaging,
            Route::Adapter,
            Os::Windows,
            volume_class(root),
        )
    }

    /// Witness-only privacy proof for one staged path. It re-reads the
    /// descriptor with `GetKernelObjectSecurity` and parses the DACL itself
    /// instead of reusing the adapter's `private_dacl`, so a fault in the
    /// descriptor the adapter writes fails the witness. A directory must carry
    /// the owner's object ACE plus the inherit-only descendant ACE; a file must
    /// carry exactly the object ACE.
    pub(crate) fn staged_entry_is_private(path: &Path, directory: bool) -> bool {
        let Some(text) = path.to_str() else {
            return false;
        };
        let access = if directory {
            DIRECTORY_ACCESS
        } else {
            GENERIC_READ
        } | READ_CONTROL;
        let Ok(file) = open_shared_path(text, access) else {
            return false;
        };
        let Ok(standard) = standard_info(&file) else {
            return false;
        };
        if standard.Directory != directory {
            return false;
        }
        let Ok((descriptor, _)) = query_security(
            &file,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
        ) else {
            return false;
        };
        let base = descriptor.as_ptr().cast_mut().cast();
        let Ok((sid, _)) = current_principal_sid() else {
            return false;
        };
        let user: PSID = sid.as_ptr().cast_mut().cast();
        let mut owner: PSID = ptr::null_mut();
        let mut defaulted = 0;
        if unsafe { GetSecurityDescriptorOwner(base, &mut owner, &mut defaulted) } == 0
            || owner.is_null()
            || unsafe { IsValidSid(owner) } == 0
            || unsafe { EqualSid(owner, user) } == 0
        {
            return false;
        }
        let mut control = 0u16;
        let mut revision = 0u32;
        if unsafe { GetSecurityDescriptorControl(base, &mut control, &mut revision) } == 0
            || control & SE_DACL_PROTECTED == 0
        {
            return false;
        }
        let mut present = 0;
        let mut acl: *mut ACL = ptr::null_mut();
        if unsafe { GetSecurityDescriptorDacl(base, &mut present, &mut acl, &mut defaulted) } == 0
            || present == 0
            || acl.is_null()
            || unsafe { IsValidAcl(acl) } == 0
        {
            return false;
        }
        let mut information = ACL_SIZE_INFORMATION::default();
        if unsafe {
            GetAclInformation(
                acl,
                (&mut information as *mut ACL_SIZE_INFORMATION).cast(),
                size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            )
        } == 0
        {
            return false;
        }
        let mut object_ace = false;
        let mut descendant_ace = false;
        for index in 0..information.AceCount {
            let mut ace: *mut std::ffi::c_void = ptr::null_mut();
            if unsafe { GetAce(acl, index, &mut ace) } == 0 || ace.is_null() {
                return false;
            }
            let header = unsafe { ptr::read(ace.cast::<ACE_HEADER>()) };
            if header.AceType != ACCESS_ALLOWED_ACE_TYPE
                || u32::from(header.AceFlags) & INHERITED_ACE != 0
            {
                return false;
            }
            let allowed = unsafe { ptr::read(ace.cast::<ACCESS_ALLOWED_ACE>()) };
            let entry: PSID = unsafe {
                ace.cast::<u8>()
                    .add(offset_of!(ACCESS_ALLOWED_ACE, SidStart))
                    .cast()
            };
            if unsafe { EqualSid(entry, user) } == 0 || allowed.Mask != FILE_ALL_ACCESS {
                return false;
            }
            let flags = u32::from(header.AceFlags);
            if flags & INHERIT_ONLY_ACE != 0 {
                if flags & (OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE) == 0 {
                    return false;
                }
                descendant_ace = true;
            } else {
                object_ace = true;
            }
        }
        if directory {
            object_ace && descendant_ace && information.AceCount == 2
        } else {
            object_ace && !descendant_ace && information.AceCount == 1
        }
    }

    fn open_existing_directory(path: &Path) -> Result<File, RootError> {
        let text = path.to_str().ok_or(RootError::TargetUnavailable)?;
        open_shared_path(text, DIRECTORY_ACCESS | READ_CONTROL)
            .map_err(|_| RootError::TargetUnavailable)
    }

    /// Path-based opens must coexist with the pinned staging handles, which
    /// grant read and write but deny delete sharing. This handle is used to
    /// re-prove identity or flush a directory; it never writes staged content.
    /// It denies delete sharing too, so a bound object cannot be replaced by a
    /// path open.
    fn open_shared_path(path: &str, access: u32) -> Result<File, u32> {
        let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                access,
                STAGE_SHARE,
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

    fn checked_components<'a>(relative: &[&'a str]) -> io::Result<Vec<&'a str>> {
        relative
            .iter()
            .map(|component| {
                if stage_component(component) {
                    Ok(*component)
                } else {
                    Err(io::Error::from_raw_os_error(ERROR_INVALID_NAME as i32))
                }
            })
            .collect()
    }

    fn stage_component(name: &str) -> bool {
        ordinary_component(name) && !name.ends_with('.') && !name.ends_with(' ')
    }

    /// Open one component relative to the pinned parent handle. `RootDirectory`
    /// is the authority, so a renamed or replaced ancestor cannot redirect this
    /// open; `FILE_OPEN_REPARSE_POINT` and `OBJ_DONT_REPARSE` refuse a link.
    #[allow(clippy::too_many_arguments)]
    fn nt_open_relative(
        parent: &File,
        name: &str,
        access: u32,
        share: u32,
        disposition: u32,
        options: u32,
        file_attributes: u32,
        security: Option<&[u64]>,
    ) -> Result<File, u32> {
        if !stage_component(name) {
            return Err(ERROR_INVALID_NAME);
        }
        let mut wide: Vec<u16> = name.encode_utf16().collect();
        let bytes = wide
            .len()
            .checked_mul(2)
            .and_then(|bytes| u16::try_from(bytes).ok())
            .ok_or(ERROR_INVALID_NAME)?;
        let unicode = UNICODE_STRING {
            Length: bytes,
            MaximumLength: bytes,
            Buffer: wide.as_mut_ptr(),
        };
        let attributes = OBJECT_ATTRIBUTES {
            Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
            RootDirectory: parent.as_raw_handle() as HANDLE,
            ObjectName: &unicode,
            Attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
            SecurityDescriptor: security.map_or(ptr::null(), |words| words.as_ptr().cast()),
            SecurityQualityOfService: ptr::null(),
        };
        let mut handle: HANDLE = ptr::null_mut();
        let mut status_block = IO_STATUS_BLOCK::default();
        let status = unsafe {
            NtCreateFile(
                &mut handle,
                access,
                &attributes,
                &mut status_block,
                ptr::null(),
                file_attributes,
                share,
                disposition,
                options,
                ptr::null(),
                0,
            )
        };
        if status < 0 {
            return Err(unsafe { RtlNtStatusToDosError(status) });
        }
        Ok(unsafe { File::from_raw_handle(handle) })
    }

    fn verify_staged_directory(file: &File, volume: u64) -> Result<(), RootError> {
        let tag = attribute_tag(file)?;
        let standard = standard_info(file)?;
        if is_reparse(&tag)
            || !standard.Directory
            || standard.DeletePending
            || file_id(file)?.VolumeSerialNumber != volume
        {
            return Err(RootError::TargetUnavailable);
        }
        Ok(())
    }

    fn verify_staged_file(file: &File, volume: u64) -> Result<(), RootError> {
        let tag = attribute_tag(file)?;
        let standard = standard_info(file)?;
        if is_reparse(&tag) || standard.Directory || file_id(file)?.VolumeSerialNumber != volume {
            return Err(RootError::TargetUnavailable);
        }
        Ok(())
    }

    /// Mark an open handle for deletion on close. The disposition is
    /// handle-relative, so no pathname is resolved for the removal.
    fn dispose(file: File) -> io::Result<()> {
        let information = FILE_DISPOSITION_INFO { DeleteFile: true };
        let ok = unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle() as HANDLE,
                FileDispositionInfo,
                (&information as *const FILE_DISPOSITION_INFO).cast(),
                size_of::<FILE_DISPOSITION_INFO>() as u32,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn open_child_error(code: u32) -> RootError {
        match code {
            ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => RootError::MissingParent,
            // Only an existing name is a boundary refusal. A permissions,
            // sharing or reparse failure leaves the tree unverified, matching
            // the Linux `EACCES -> Io` mapping.
            ERROR_FILE_EXISTS | ERROR_ALREADY_EXISTS => RootError::TargetUnavailable,
            _ => RootError::Io,
        }
    }

    fn nt_io(code: u32) -> io::Error {
        io::Error::from_raw_os_error(code as i32)
    }

    fn root_io(error: RootError) -> io::Error {
        io::Error::other(error)
    }

    /// The SID this process assigns as owner to new objects: the token's
    /// default owner. It is the user SID for an ordinary token and the
    /// Administrators group for an elevated administrator token, which is the
    /// principal that also owns directories created by the storage catalog.
    fn current_principal_sid() -> Result<(Vec<u32>, u32), RootError> {
        let mut token: HANDLE = ptr::null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(RootError::Io);
        }
        let mut needed = 0u32;
        unsafe { GetTokenInformation(token, TokenOwner, ptr::null_mut(), 0, &mut needed) };
        if needed == 0 {
            unsafe { CloseHandle(token) };
            return Err(RootError::Io);
        }
        let mut buffer = vec![0u64; (needed as usize).div_ceil(size_of::<u64>())];
        let ok = unsafe {
            GetTokenInformation(
                token,
                TokenOwner,
                buffer.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        };
        if ok == 0 {
            unsafe { CloseHandle(token) };
            return Err(RootError::Io);
        }
        let owner = unsafe { ptr::read(buffer.as_ptr().cast::<TOKEN_OWNER>()) };
        let length = unsafe { GetLengthSid(owner.Owner) };
        if length == 0 || length > needed {
            unsafe { CloseHandle(token) };
            return Err(RootError::Io);
        }
        let mut sid = vec![0u32; (length as usize).div_ceil(size_of::<u32>())];
        let copied = unsafe { CopySid(length, sid.as_mut_ptr().cast(), owner.Owner) };
        unsafe { CloseHandle(token) };
        if copied == 0 {
            return Err(RootError::Io);
        }
        Ok((sid, length))
    }

    fn owned_by_current_user(file: &File) -> Result<bool, RootError> {
        let (descriptor, _) = query_security(file, OWNER_SECURITY_INFORMATION)?;
        let base = descriptor.as_ptr().cast_mut().cast();
        let mut owner: PSID = ptr::null_mut();
        let mut defaulted = 0;
        if unsafe { GetSecurityDescriptorOwner(base, &mut owner, &mut defaulted) } == 0
            || owner.is_null()
            || unsafe { IsValidSid(owner) } == 0
        {
            return Ok(false);
        }
        let (sid, _) = current_principal_sid()?;
        Ok(unsafe { EqualSid(owner, sid.as_ptr().cast_mut().cast()) } != 0)
    }

    /// Owner-only privacy: a protected DACL whose every ACE names the current
    /// user, with at least one full-control ACE for the object itself. This is
    /// the mode-`0700` equivalent; inherited ACEs are refused by construction.
    fn private_dacl(file: &File) -> Result<bool, RootError> {
        let (descriptor, _) =
            query_security(file, OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION)?;
        let base = descriptor.as_ptr().cast_mut().cast();
        let mut owner: PSID = ptr::null_mut();
        let mut defaulted = 0;
        if unsafe { GetSecurityDescriptorOwner(base, &mut owner, &mut defaulted) } == 0
            || owner.is_null()
            || unsafe { IsValidSid(owner) } == 0
        {
            return Ok(false);
        }
        let (sid, _) = current_principal_sid()?;
        let user: PSID = sid.as_ptr().cast_mut().cast();
        if unsafe { EqualSid(owner, user) } == 0 {
            return Ok(false);
        }
        let mut control = 0u16;
        let mut revision = 0u32;
        if unsafe { GetSecurityDescriptorControl(base, &mut control, &mut revision) } == 0
            || control & SE_DACL_PROTECTED == 0
        {
            return Ok(false);
        }
        let mut present = 0;
        let mut acl: *mut ACL = ptr::null_mut();
        if unsafe { GetSecurityDescriptorDacl(base, &mut present, &mut acl, &mut defaulted) } == 0
            || present == 0
            || acl.is_null()
            || unsafe { IsValidAcl(acl) } == 0
        {
            return Ok(false);
        }
        let mut information = ACL_SIZE_INFORMATION::default();
        if unsafe {
            GetAclInformation(
                acl,
                (&mut information as *mut ACL_SIZE_INFORMATION).cast(),
                size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            )
        } == 0
        {
            return Ok(false);
        }
        let mut full_control = false;
        for index in 0..information.AceCount {
            let mut ace: *mut std::ffi::c_void = ptr::null_mut();
            if unsafe { GetAce(acl, index, &mut ace) } == 0 || ace.is_null() {
                return Ok(false);
            }
            let header = unsafe { ptr::read(ace.cast::<ACE_HEADER>()) };
            if header.AceType != ACCESS_ALLOWED_ACE_TYPE
                || u32::from(header.AceFlags) & INHERITED_ACE != 0
            {
                return Ok(false);
            }
            let allowed = unsafe { ptr::read(ace.cast::<ACCESS_ALLOWED_ACE>()) };
            let entry: PSID = unsafe {
                ace.cast::<u8>()
                    .add(offset_of!(ACCESS_ALLOWED_ACE, SidStart))
                    .cast()
            };
            if unsafe { EqualSid(entry, user) } == 0 {
                return Ok(false);
            }
            if u32::from(header.AceFlags) & INHERIT_ONLY_ACE == 0 && allowed.Mask == FILE_ALL_ACCESS
            {
                full_control = true;
            }
        }
        Ok(full_control)
    }

    /// Build a self-relative security descriptor that grants only the current
    /// user. A directory also gets an inherit-only ACE so its descendants stay
    /// owner-only; the DACL is protected, so no parent ACE is merged in.
    fn private_security_descriptor(directory: bool) -> Result<Vec<u64>, RootError> {
        let (sid, sid_length) = current_principal_sid()?;
        let user: PSID = sid.as_ptr().cast_mut().cast();
        let ace_length = size_of::<ACCESS_ALLOWED_ACE>() - size_of::<u32>() + sid_length as usize;
        let acl_length = size_of::<ACL>() + ace_length * if directory { 2 } else { 1 };
        let mut acl_words = vec![0u64; acl_length.div_ceil(size_of::<u64>())];
        let acl = acl_words.as_mut_ptr().cast::<ACL>();
        if unsafe { InitializeAcl(acl, acl_length as u32, ACL_REVISION) } == 0
            || unsafe { AddAccessAllowedAceEx(acl, ACL_REVISION, 0, FILE_ALL_ACCESS, user) } == 0
        {
            return Err(RootError::Io);
        }
        if directory {
            let inherit = OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE | INHERIT_ONLY_ACE;
            if unsafe { AddAccessAllowedAceEx(acl, ACL_REVISION, inherit, FILE_ALL_ACCESS, user) }
                == 0
            {
                return Err(RootError::Io);
            }
        }

        let mut descriptor = SECURITY_DESCRIPTOR::default();
        let absolute: *mut std::ffi::c_void = (&mut descriptor as *mut SECURITY_DESCRIPTOR).cast();
        if unsafe { InitializeSecurityDescriptor(absolute, SECURITY_DESCRIPTOR_REVISION) } == 0
            || unsafe { SetSecurityDescriptorOwner(absolute, user, 0) } == 0
            || unsafe { SetSecurityDescriptorDacl(absolute, 1, acl, 0) } == 0
            || unsafe {
                SetSecurityDescriptorControl(absolute, SE_DACL_PROTECTED, SE_DACL_PROTECTED)
            } == 0
        {
            return Err(RootError::Io);
        }

        let mut needed = 0u32;
        unsafe { MakeSelfRelativeSD(absolute, ptr::null_mut(), &mut needed) };
        if needed == 0 {
            return Err(RootError::Io);
        }
        let mut self_relative = vec![0u64; (needed as usize).div_ceil(size_of::<u64>())];
        if unsafe { MakeSelfRelativeSD(absolute, self_relative.as_mut_ptr().cast(), &mut needed) }
            == 0
        {
            return Err(RootError::Io);
        }
        Ok(self_relative)
    }

    #[cfg(test)]
    mod security_digest_tests {
        use super::*;
        use windows_sys::Win32::Security::{
            AddAccessAllowedAceEx, AddAccessDeniedAceEx, SetSecurityDescriptorDacl,
            SetSecurityDescriptorGroup, SetSecurityDescriptorOwner,
        };
        use windows_sys::Win32::Storage::FileSystem::{FILE_ALL_ACCESS, FILE_GENERIC_READ};

        /// A 4-byte-aligned SID in the NT authority with the given
        /// subauthorities.
        fn sid_nt(subauths: &[u32]) -> Vec<u32> {
            let length = 8 + subauths.len() * 4;
            let mut words = vec![0u32; length.div_ceil(size_of::<u32>())];
            let bytes =
                unsafe { std::slice::from_raw_parts_mut(words.as_mut_ptr().cast::<u8>(), length) };
            bytes[0] = 1; // SID_REVISION
            bytes[1] = subauths.len() as u8;
            bytes[7] = 5; // SECURITY_NT_AUTHORITY, big-endian
            for (index, subauth) in subauths.iter().enumerate() {
                bytes[8 + index * 4..12 + index * 4].copy_from_slice(&subauth.to_le_bytes());
            }
            words
        }

        /// A one-subauthority NT SID, such as `18` for Local System.
        fn sid(subauth: u32) -> Vec<u32> {
            sid_nt(&[subauth])
        }

        /// `slack` extra words are allocated inside `AclSize`, so the stored
        /// descriptor differs by padding without changing any ACE. `deny`
        /// chooses the ACE class; order is preserved as supplied.
        fn acl_with(entries: &[(bool, u8, u32, &[u32])], slack: usize) -> Vec<u32> {
            let bytes = size_of::<ACL>() + entries.len() * (8 + 12) + slack * size_of::<u32>();
            let mut words = vec![0u32; bytes.div_ceil(size_of::<u32>())];
            let acl: *mut ACL = words.as_mut_ptr().cast();
            assert_ne!(unsafe { InitializeAcl(acl, bytes as u32, ACL_REVISION) }, 0);
            for (deny, flags, mask, sid) in entries {
                let added = if *deny {
                    unsafe {
                        AddAccessDeniedAceEx(
                            acl,
                            ACL_REVISION,
                            u32::from(*flags),
                            *mask,
                            sid.as_ptr().cast_mut().cast(),
                        )
                    }
                } else {
                    unsafe {
                        AddAccessAllowedAceEx(
                            acl,
                            ACL_REVISION,
                            u32::from(*flags),
                            *mask,
                            sid.as_ptr().cast_mut().cast(),
                        )
                    }
                };
                assert_ne!(added, 0);
            }
            words
        }

        fn acl(entries: &[(u8, u32, &[u32])], slack: usize) -> Vec<u32> {
            let entries: Vec<(bool, u8, u32, &[u32])> = entries
                .iter()
                .map(|(flags, mask, sid)| (false, *flags, *mask, *sid))
                .collect();
            acl_with(&entries, slack)
        }

        /// The access a trustee actually receives for `mask`, evaluated
        /// locally over the ordered ACE list: the first entry matching the
        /// requested bits decides them (deny clears, allow sets), and later
        /// entries cannot re-grant bits already denied.
        fn effective(entries: &[(bool, u8, u32, &[u32])], sid: &[u32], mask: u32) -> u32 {
            let mut granted = 0u32;
            let mut decided = 0u32;
            for (deny, _flags, ace_mask, ace_sid) in entries {
                if *ace_sid != sid {
                    continue;
                }
                let applicable = *ace_mask & mask & !decided;
                if applicable == 0 {
                    continue;
                }
                decided |= applicable;
                if *deny {
                    granted &= !applicable;
                } else {
                    granted |= applicable;
                }
            }
            granted & mask
        }

        fn descriptor(owner: &[u32], group: &[u32], acl: &[u32]) -> SECURITY_DESCRIPTOR {
            let mut descriptor = SECURITY_DESCRIPTOR::default();
            let base = (&mut descriptor as *mut SECURITY_DESCRIPTOR).cast();
            assert_ne!(
                unsafe { InitializeSecurityDescriptor(base, SECURITY_DESCRIPTOR_REVISION) },
                0
            );
            assert_ne!(
                unsafe { SetSecurityDescriptorOwner(base, owner.as_ptr().cast_mut().cast(), 0) },
                0
            );
            assert_ne!(
                unsafe { SetSecurityDescriptorGroup(base, group.as_ptr().cast_mut().cast(), 0) },
                0
            );
            assert_ne!(
                unsafe { SetSecurityDescriptorDacl(base, 1, acl.as_ptr().cast(), 0) },
                0
            );
            descriptor
        }

        fn digest(descriptor: &mut SECURITY_DESCRIPTOR) -> [u8; 32] {
            security_digest((descriptor as *mut SECURITY_DESCRIPTOR).cast()).unwrap()
        }

        #[test]
        fn layout_and_provenance_are_ignored() {
            let owner = sid(18);
            let group = sid(18);
            let system = sid(18);
            let user = sid(544);
            let entries = [
                (0, FILE_ALL_ACCESS, user.as_slice()),
                (0, FILE_ALL_ACCESS, system.as_slice()),
            ];
            let first_acl = acl(&entries, 0);
            // Same ACE order, different padding and the provenance-only
            // inherited flag set on one ACE.
            let mut second_acl = acl(&entries, 8);
            let second_bytes =
                unsafe { std::slice::from_raw_parts_mut(second_acl.as_mut_ptr().cast::<u8>(), 10) };
            second_bytes[9] |= INHERITED_ACE as u8;

            let mut first = descriptor(&owner, &group, &first_acl);
            let mut second = descriptor(&owner, &group, &second_acl);
            assert_eq!(digest(&mut first), digest(&mut second));
        }

        #[test]
        fn ace_order_changes_the_digest() {
            let owner = sid(18);
            let group = sid(18);
            let system = sid(18);
            let user = sid(544);
            let first_acl = acl(
                &[(0, FILE_ALL_ACCESS, &user), (0, FILE_ALL_ACCESS, &system)],
                0,
            );
            let second_acl = acl(
                &[(0, FILE_ALL_ACCESS, &system), (0, FILE_ALL_ACCESS, &user)],
                0,
            );

            let mut first = descriptor(&owner, &group, &first_acl);
            let mut second = descriptor(&owner, &group, &second_acl);
            assert_ne!(
                digest(&mut first),
                digest(&mut second),
                "the digest must preserve evaluation order"
            );
        }

        #[test]
        fn digest_distinguishes_the_effective_access_of_overlapping_aces() {
            let owner = sid(18);
            let group = sid(18);
            let user = sid(544);
            // The same two ACEs in both orders. Windows evaluates the first
            // matching ACE, so one DACL allows the read and the other denies
            // it; the digest must separate those grants.
            let deny_entries = [
                (true, 0, FILE_GENERIC_READ, user.as_slice()),
                (false, 0, FILE_GENERIC_READ, user.as_slice()),
            ];
            let deny_acl = acl_with(&deny_entries, 0);
            let allow_entries = [
                (false, 0, FILE_GENERIC_READ, user.as_slice()),
                (true, 0, FILE_GENERIC_READ, user.as_slice()),
            ];
            let allow_acl = acl_with(&allow_entries, 0);

            assert_eq!(effective(&deny_entries, &user, FILE_GENERIC_READ), 0);
            assert_eq!(
                effective(&allow_entries, &user, FILE_GENERIC_READ),
                FILE_GENERIC_READ
            );

            let mut deny = descriptor(&owner, &group, &deny_acl);
            let mut allow = descriptor(&owner, &group, &allow_acl);
            assert_ne!(digest(&mut deny), digest(&mut allow));
        }

        #[test]
        fn widened_dacl_hashes_differently() {
            let owner = sid(18);
            let group = sid(18);
            let user = sid(544);
            let everyone = sid(545);
            let base_acl = acl(&[(0, FILE_GENERIC_READ, &user)], 0);
            let widened_mask = acl(&[(0, FILE_ALL_ACCESS, &user)], 0);
            let added_ace = acl(
                &[
                    (0, FILE_GENERIC_READ, &user),
                    (0, FILE_GENERIC_READ, &everyone),
                ],
                0,
            );

            let mut base = descriptor(&owner, &group, &base_acl);
            let mut mask = descriptor(&owner, &group, &widened_mask);
            let mut added = descriptor(&owner, &group, &added_ace);
            assert_ne!(digest(&mut base), digest(&mut mask));
            assert_ne!(digest(&mut base), digest(&mut added));
        }

        #[test]
        fn changed_owner_hashes_differently() {
            let group = sid(18);
            let system = sid(18);
            // BUILTIN\Administrators, the elevated token's default owner. It is
            // a different well-known SID from Local System, so the comparison
            // cannot resolve to one owner through the token.
            let administrators = sid_nt(&[32, 544]);
            let user = sid(544);
            let acl = acl(&[(0, FILE_ALL_ACCESS, &user)], 0);

            let mut first = descriptor(&system, &group, &acl);
            let mut second = descriptor(&administrators, &group, &acl);
            assert_ne!(digest(&mut first), digest(&mut second));
        }
    }
}

pub(crate) use root::{
    Binding, PrivateFile, ResolvedTarget, StagedDirectory, StagingTree, bind_installation,
    directory_identity, fingerprint_path, observe_target, open_or_create_private_file,
    open_private_child_directory, open_private_directory, private_child_path, reopen_installation,
    resolve_existing_parent, staged_entry_is_private, staging_volume_qualified,
    verify_private_directory, volume_class,
};

/// Product library import on Windows is answered by the record: it needs the
/// catalog layer, not only the staging adapter. The class comes from the
/// opened root; `marduk_core::qualification` decides.
pub(crate) fn library_volume_qualified(root: &Path) -> bool {
    available(
        Capability::LibraryStaging,
        Route::Import,
        Os::Windows,
        volume_class(root),
    )
}

/// The composed native effect adapter on Windows is answered by the record. It
/// needs the adapter layer, which the NTFS composition proof satisfies.
pub(crate) fn effects_volume_qualified(root: &Path) -> bool {
    available(
        Capability::NativeEffects,
        Route::Adapter,
        Os::Windows,
        volume_class(root),
    )
}

/// Client deploy and restore on Windows require the composition proof, which
/// the NTFS whole-composition run records.
pub(crate) fn deploy_volume_qualified(root: &Path) -> bool {
    available(
        Capability::NativeEffects,
        Route::Deploy,
        Os::Windows,
        volume_class(root),
    )
}
