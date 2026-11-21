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
