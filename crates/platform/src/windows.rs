//! Windows writer gate: a kept `CreateFileW` handle locking byte zero.

use std::fs::File;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{
    ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION, GENERIC_READ, GENERIC_WRITE, GetLastError,
    HANDLE, INVALID_HANDLE_VALUE, WIN32_ERROR,
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
            volume,
            index,
        })
    }

    /// Take the exclusive lock for this entrant.
    ///
    /// There is no delete sharing, so another entrant cannot replace the lock
    /// file while it is held. Each entrant opens its own handle.
    pub fn try_lock(&self) -> Result<WriterGuard, GateError> {
        let file = open_lock_file(&self.path, false)?;
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
// Every object is opened against a volume-qualified GUID path with
// `FILE_FLAG_OPEN_REPARSE_POINT`, then verified by handle information. Reparse
// tags are refused rather than followed, and `GetFinalPathNameByHandleW` plus
// the 64-bit volume serial from `FileIdInfo` prove containment. Handles pin
// names without delete sharing.
//
// TOCTOU limit: observation is not write authority. The pinned parent identity
// and its recorded locator are rechecked, the leaf is re-enumerated from that
// pinned handle, and the opened leaf's `FILE_ID_INFO` must equal the enumerated
// entry before hashing. A rename of an ancestor changes the pinned parent's
// final path and is refused. A rename after those checks cannot redirect the
// opened object to unverified bytes (opened identity is compared again), but a
// writer with shared access can still change bytes after the final hash; a
// later native effect must re-observe under the same pinned handles.
// ---------------------------------------------------------------------------
mod root {
    use std::fs::File;
    use std::io::Read;
    use std::mem::offset_of;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use std::path::Path;
    use std::ptr;

    use marduk_core::root_identity::{
        FileMetadata, Fingerprint, GameLayout, NativeIdentity, RootError, RootIdentity,
        TargetObservation, UnsupportedReason,
    };
    use marduk_core::targets::RelativePath;
    use sha2::{Digest, Sha256};
    use windows_sys::Win32::Foundation::{
        ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_HANDLE_EOF, ERROR_INSUFFICIENT_BUFFER,
        ERROR_LOCK_VIOLATION, ERROR_MORE_DATA, ERROR_NO_MORE_FILES, ERROR_PATH_NOT_FOUND,
        ERROR_SHARING_VIOLATION, GENERIC_READ, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, GROUP_SECURITY_INFORMATION, GetKernelObjectSecurity,
        OWNER_SECURITY_INFORMATION,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_COMPRESSED, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_ENCRYPTED,
        FILE_ATTRIBUTE_OFFLINE, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_SPARSE_FILE,
        FILE_ATTRIBUTE_TAG_INFO, FILE_BASIC_INFO, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_EXTD_DIR_INFO, FILE_ID_INFO,
        FILE_INFO_BY_HANDLE_CLASS, FILE_NAME_NORMALIZED, FILE_SHARE_READ, FILE_SHARE_WRITE,
        FILE_STANDARD_INFO, FILE_STREAM_INFO, FileAttributeTagInfo, FileBasicInfo,
        FileIdExtdDirectoryInfo, FileIdExtdDirectoryRestartInfo, FileIdInfo, FileStandardInfo,
        FileStreamInfo, GetFileInformationByHandleEx, GetFinalPathNameByHandleW,
        GetVolumeInformationByHandleW, OPEN_EXISTING, VOLUME_NAME_GUID,
    };

    pub(crate) struct RootDir {
        file: File,
        final_path: String,
        volume: u64,
        identity: RootIdentity,
    }

    pub(crate) struct Binding {
        installation: RootDir,
        data: RootDir,
    }

    impl Binding {
        pub(crate) fn identities(&self) -> (&RootIdentity, &RootIdentity) {
            (&self.installation.identity, &self.data.identity)
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

    pub(crate) struct ResolvedTarget<'a> {
        /// Held without delete sharing so the resolved parent cannot be
        /// renamed under the pending observation.
        parent: File,
        /// Locator observed at resolution; a later ancestor rename changes it.
        parent_path: String,
        parent_identity: RootIdentity,
        parent_volume: u64,
        leaf: String,
        /// Identity enumerated from the pinned parent when the leaf existed.
        leaf_identity: Option<NativeIdentity>,
        _binding: &'a Binding,
    }

    impl ResolvedTarget<'_> {
        pub(crate) fn parent_identity(&self) -> &RootIdentity {
            &self.parent_identity
        }

        pub(crate) fn leaf(&self) -> &str {
            &self.leaf
        }
    }

    pub(crate) fn bind_installation(
        selection: &Path,
        layout: &GameLayout,
    ) -> Result<Binding, RootError> {
        let installation_file = open_path(selection).map_err(|_| RootError::TargetUnavailable)?;
        let installation = verify_root(installation_file)?;

        if !ordinary_component(layout.data) {
            return Err(RootError::UnsupportedTarget(
                UnsupportedReason::Configuration,
            ));
        }
        let data_path = selection.join(layout.data);
        let data_file = open_path(&data_path)
            .map_err(|_| RootError::UnsupportedTarget(UnsupportedReason::Configuration))?;
        let data = verify_root(data_file)
            .map_err(|_| RootError::UnsupportedTarget(UnsupportedReason::Configuration))?;
        if !same_domain(&installation, &data) || installation.identity == data.identity {
            return Err(RootError::UnsupportedTarget(
                UnsupportedReason::Configuration,
            ));
        }
        if !has_marker(&installation, layout)? {
            return Err(RootError::UnsupportedTarget(
                UnsupportedReason::Configuration,
            ));
        }

        Ok(Binding { installation, data })
    }

    pub(crate) fn reopen_installation(
        location: &Path,
        layout: &GameLayout,
        installation: &RootIdentity,
        data: &RootIdentity,
    ) -> Result<Binding, RootError> {
        let reopened = reopen_pair(location, layout).map_err(|_| RootError::TargetUnavailable)?;
        if reopened.installation.identity != *installation || reopened.data.identity != *data {
            return Err(RootError::TargetUnavailable);
        }
        Ok(reopened)
    }

    fn reopen_pair(location: &Path, layout: &GameLayout) -> Result<Binding, RootError> {
        let installation_file = open_path(location).map_err(|_| RootError::TargetUnavailable)?;
        let installation = verify_root(installation_file)?;
        if !ordinary_component(layout.data) {
            return Err(RootError::TargetUnavailable);
        }
        let data_file =
            open_path(&location.join(layout.data)).map_err(|_| RootError::TargetUnavailable)?;
        let data = verify_root(data_file)?;
        if !same_domain(&installation, &data) {
            return Err(RootError::TargetUnavailable);
        }
        Ok(Binding { installation, data })
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

        let mut parent = binding.data.file.try_clone().map_err(|_| RootError::Io)?;
        let mut parent_path = binding.data.final_path.clone();
        let mut parent_identity = binding.data.identity.clone();
        let mut parent_volume = binding.data.volume;

        for component in components {
            let name = single_match(&parent, component)?;
            let opened = open_child(&parent_path, &name)?;
            let tag = attribute_tag(&opened)?;
            if is_reparse(&tag) {
                return Err(RootError::OutsideBoundary);
            }
            let standard = standard_info(&opened)?;
            if !standard.Directory {
                return Err(RootError::UnsupportedTarget(UnsupportedReason::ObjectKind));
            }
            ensure_within(&binding.data, &opened)?;

            parent_path = final_path(&opened)?;
            parent_identity = RootIdentity::from_adapter_evidence(identity(&opened)?);
            parent_volume = file_id(&opened)?.VolumeSerialNumber;
            parent = opened;
        }

        let (leaf, leaf_identity) = match leaf_match(&parent, &leaf_planned)? {
            Some(entry) => {
                let opened = open_child(&parent_path, &entry.name)?;
                if is_reparse(&attribute_tag(&opened)?) {
                    return Err(RootError::OutsideBoundary);
                }
                let expected = entry_identity(parent_volume, &entry);
                if identity(&opened)? != expected {
                    return Err(RootError::TargetUnavailable);
                }
                (entry.name, Some(expected))
            }
            None => (leaf_planned, None),
        };

        Ok(ResolvedTarget {
            parent,
            parent_path,
            parent_identity,
            parent_volume,
            leaf,
            leaf_identity,
            _binding: binding,
        })
    }

    pub(crate) fn observe_target(
        target: &ResolvedTarget<'_>,
    ) -> Result<TargetObservation, RootError> {
        // The pinned parent must still be the object recorded by resolution.
        if identity(&target.parent).map_err(|_| RootError::TargetUnavailable)?
            != *target.parent_identity.evidence()
        {
            return Err(RootError::TargetUnavailable);
        }
        // An ancestor rename or replacement changes the pinned parent's
        // locator; the recorded path is then stale authority.
        let current_path = final_path(&target.parent).map_err(|_| RootError::TargetUnavailable)?;
        if !current_path.eq_ignore_ascii_case(&target.parent_path) {
            return Err(RootError::TargetUnavailable);
        }

        // Re-enumerate from the pinned parent, not from a path string.
        let mut found = matches(&target.parent, &target.leaf)?;
        if found.is_empty() {
            return Ok(TargetObservation::Absent);
        }
        if found.len() > 1 {
            return Err(RootError::AmbiguousName);
        }
        let entry = found.remove(0);
        let enumerated = entry_identity(target.parent_volume, &entry);
        if target
            .leaf_identity
            .as_ref()
            .is_some_and(|expected| *expected != enumerated)
        {
            return Err(RootError::TargetUnavailable);
        }

        let path = join(&current_path, &entry.name);
        let file = match open_path(Path::new(&path)) {
            Ok(file) => file,
            Err(code) if is_missing(code) => return Ok(TargetObservation::Absent),
            Err(code) if is_sharing(code) => {
                return Ok(TargetObservation::Unsupported {
                    reason: UnsupportedReason::Sharing,
                });
            }
            Err(_) => return Err(RootError::Io),
        };
        if identity(&file).map_err(|_| RootError::Io)? != enumerated {
            return Err(RootError::TargetUnavailable);
        }

        let tag = attribute_tag(&file)?;
        if is_reparse(&tag) {
            return Ok(TargetObservation::Unsupported {
                reason: UnsupportedReason::ObjectKind,
            });
        }
        let standard = standard_info(&file)?;
        if standard.Directory {
            return Ok(TargetObservation::Unsupported {
                reason: UnsupportedReason::ObjectKind,
            });
        }
        if standard.NumberOfLinks > 1 {
            return Ok(TargetObservation::Unsupported {
                reason: UnsupportedReason::Metadata,
            });
        }
        ensure_within(&target._binding.data, &file)?;

        let basic = basic_info(&file)?;
        if basic.FileAttributes
            & (FILE_ATTRIBUTE_COMPRESSED
                | FILE_ATTRIBUTE_ENCRYPTED
                | FILE_ATTRIBUTE_OFFLINE
                | FILE_ATTRIBUTE_SPARSE_FILE
                | FILE_ATTRIBUTE_REPARSE_POINT)
            != 0
        {
            return Ok(TargetObservation::Unsupported {
                reason: UnsupportedReason::Metadata,
            });
        }
        if has_unknown_streams(&file)? {
            return Ok(TargetObservation::Unsupported {
                reason: UnsupportedReason::Metadata,
            });
        }

        let before_identity = identity(&file)?;
        let before_basic = basic;
        let security_sha256 = security_sha256(&file)?;

        let mut hasher = Sha256::new();
        let mut reader = &file;
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

        let after_identity = identity(&file)?;
        let after_basic = basic_info(&file)?;
        let after_standard = standard_info(&file)?;
        if before_identity != after_identity
            || before_basic.LastWriteTime != after_basic.LastWriteTime
            || before_basic.ChangeTime != after_basic.ChangeTime
            || len != standard.EndOfFile as u64
            || after_standard.EndOfFile != standard.EndOfFile
        {
            return Err(RootError::Io);
        }

        let mut sha256 = [0u8; 32];
        sha256.copy_from_slice(&hasher.finalize());
        Ok(TargetObservation::Regular {
            fingerprint: Fingerprint {
                identity: before_identity,
                len,
                sha256,
                metadata: FileMetadata::Windows {
                    attributes: before_basic.FileAttributes,
                    last_write_100ns: before_basic.LastWriteTime,
                    links: standard.NumberOfLinks,
                    security_sha256,
                },
            },
        })
    }

    fn open_path(path: &Path) -> Result<File, u32> {
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(unsafe { GetLastError() });
        }
        Ok(unsafe { File::from_raw_handle(handle) })
    }

    fn open_child(parent_path: &str, name: &str) -> Result<File, RootError> {
        open_path(Path::new(&join(parent_path, name))).map_err(|code| {
            if is_missing(code) {
                RootError::MissingParent
            } else if is_sharing(code) {
                RootError::UnsupportedTarget(UnsupportedReason::Sharing)
            } else {
                RootError::Io
            }
        })
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
        if !filesystem_is_ntfs(&file)? {
            return Err(RootError::UnsupportedTarget(
                UnsupportedReason::Configuration,
            ));
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

    fn same_domain(installation: &RootDir, data: &RootDir) -> bool {
        installation.volume == data.volume && is_within(&data.final_path, &installation.final_path)
    }

    fn ensure_within(root: &RootDir, file: &File) -> Result<(), RootError> {
        let id = file_id(file)?;
        if id.VolumeSerialNumber != root.volume {
            return Err(RootError::OutsideBoundary);
        }
        let path = final_path(file)?;
        if !is_within(&path, &root.final_path) {
            return Err(RootError::OutsideBoundary);
        }
        Ok(())
    }

    fn is_within(child: &str, parent: &str) -> bool {
        let child_lower = child.to_lowercase();
        let parent_lower = parent.trim_end_matches('\\').to_lowercase();
        child_lower
            .strip_prefix(&parent_lower)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('\\'))
    }

    fn identity(file: &File) -> Result<NativeIdentity, RootError> {
        let id = file_id(file)?;
        let basic = basic_info(file)?;
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

    fn filesystem_is_ntfs(file: &File) -> Result<bool, RootError> {
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
            .unwrap_or(name.len());
        Ok(String::from_utf16_lossy(&name[..end]).eq_ignore_ascii_case("NTFS"))
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
                return Ok(String::from_utf16_lossy(&buffer));
            }
            buffer = vec![0u16; len as usize + 1];
        }
    }

    fn read_directory(directory: &File) -> Result<Vec<DirectoryEntry>, RootError> {
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

    fn single_match(directory: &File, component: &str) -> Result<String, RootError> {
        let mut found = matches(directory, component)?;
        match found.len() {
            0 => Err(RootError::MissingParent),
            1 => Ok(found.remove(0).name),
            _ => Err(RootError::AmbiguousName),
        }
    }

    fn leaf_match(directory: &File, component: &str) -> Result<Option<DirectoryEntry>, RootError> {
        let mut found = matches(directory, component)?;
        match found.len() {
            0 => Ok(None),
            1 => Ok(Some(found.remove(0))),
            _ => Err(RootError::AmbiguousName),
        }
    }

    fn matches(directory: &File, component: &str) -> Result<Vec<DirectoryEntry>, RootError> {
        Ok(read_directory(directory)?
            .into_iter()
            .filter(|entry| entry.name.eq_ignore_ascii_case(component))
            .collect())
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
        let requested =
            OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
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

        let mut descriptor = vec![0u8; needed as usize];
        let ok = unsafe {
            GetKernelObjectSecurity(
                file.as_raw_handle() as HANDLE,
                requested,
                descriptor.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        };
        if ok == 0 {
            return Err(RootError::Io);
        }
        let mut sha256 = [0u8; 32];
        let mut hasher = Sha256::new();
        hasher.update(&descriptor[..needed as usize]);
        sha256.copy_from_slice(&hasher.finalize());
        Ok(sha256)
    }

    fn is_missing(code: u32) -> bool {
        matches!(code, ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND)
    }

    fn is_sharing(code: u32) -> bool {
        matches!(
            code,
            ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION | ERROR_ACCESS_DENIED
        )
    }
}

pub(crate) use root::{
    Binding, ResolvedTarget, bind_installation, observe_target, reopen_installation,
    resolve_existing_parent,
};
