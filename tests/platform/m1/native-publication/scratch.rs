use std::fs::{self, File};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

pub fn scratch() -> Option<(tempfile::TempDir, &'static str)> {
    let Some(parent) = std::env::var_os("MARDUK_R03_SCRATCH") else {
        eprintln!("R03 NOT RUN: MARDUK_R03_SCRATCH not set");
        return None;
    };
    let parent = fs::canonicalize(parent).expect("configured scratch parent");
    if !ordinary_user() {
        eprintln!("R03 NOT RUN: ordinary, non-elevated user not established");
        return None;
    }
    let Some(fs) = filesystem(&parent) else {
        eprintln!("R03 NOT RUN: scratch filesystem not in this runtime matrix");
        return None;
    };
    let dir = tempfile::Builder::new()
        .prefix("marduk-r03-")
        .tempdir_in(parent)
        .unwrap();
    assert_eq!(filesystem(dir.path()), Some(fs));
    eprintln!("R03 scratch: {fs} {:?}", dir.path());
    Some((dir, fs))
}

#[cfg(target_os = "linux")]
fn ordinary_user() -> bool {
    (unsafe { libc::geteuid() != 0 })
        && fs::read_to_string("/proc/self/status")
            .unwrap()
            .lines()
            .any(|line| {
                line.strip_prefix("CapEff:")
                    .is_some_and(|v| u64::from_str_radix(v.trim(), 16) == Ok(0))
            })
}

#[cfg(target_os = "linux")]
pub fn filesystem(path: &Path) -> Option<&'static str> {
    use std::os::fd::AsRawFd;

    let f = File::open(path).unwrap();
    let mut info = std::mem::MaybeUninit::<libc::statfs>::uninit();
    assert_eq!(
        unsafe { libc::fstatfs(f.as_raw_fd(), info.as_mut_ptr()) },
        0
    );
    match unsafe { info.assume_init() }.f_type as libc::c_long {
        libc::BTRFS_SUPER_MAGIC => Some("btrfs"),
        libc::TMPFS_MAGIC => Some("tmpfs"),
        _ => None,
    }
}

#[cfg(windows)]
fn ordinary_user() -> bool {
    let groups = Command::new("whoami")
        .args(["/groups", "/fo", "csv"])
        .output()
        .unwrap();
    let groups = String::from_utf8_lossy(&groups.stdout);
    groups.contains("S-1-16-8192")
        && !groups.contains("S-1-16-12288")
        && !groups.contains("S-1-16-16384")
}

#[cfg(windows)]
pub fn filesystem(path: &Path) -> Option<&'static str> {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, GetVolumeInformationByHandleW,
    };

    let dir = fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .unwrap();
    let mut kind = [0u16; 32];
    let mut serial = 0;
    let ok = unsafe {
        GetVolumeInformationByHandleW(
            dir.as_raw_handle(),
            std::ptr::null_mut(),
            0,
            &mut serial,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            kind.as_mut_ptr(),
            kind.len() as u32,
        )
    };
    let ntfs = ok != 0 && kind.starts_with(&[78, 84, 70, 83, 0]);
    if ntfs {
        eprintln!("R03 NTFS volume serial: {serial:08x}");
    }
    ntfs.then_some("ntfs")
}

pub struct Worker(Child);

impl Worker {
    pub fn spawn(test: &str, mode: &str, scratch: &Path) -> Self {
        Self(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", test, "--ignored", "--nocapture"])
                .env("MARDUK_R03_CHILD_MODE", mode)
                .env("MARDUK_R03_CHILD_ROOT", scratch)
                .stdin(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }

    pub fn checkpoint(&mut self, file: &Path) {
        let until = Instant::now() + Duration::from_secs(30);
        while !file.is_file() {
            assert!(
                self.0.try_wait().unwrap().is_none(),
                "child exited before {file:?}"
            );
            assert!(Instant::now() < until, "child timed out at {file:?}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn finish(mut self) {
        let until = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                assert!(status.success(), "{status}");
                return;
            }
            assert!(Instant::now() < until, "child timed out");
            thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn interrupt(mut self) {
        self.0.kill().unwrap();
        assert!(!self.0.wait().unwrap().success());
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if self.0.try_wait().is_ok_and(|status| status.is_none()) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

pub fn pause(root: &Path) -> ! {
    File::options()
        .write(true)
        .create_new(true)
        .open(root.join("child-ready"))
        .unwrap();
    loop {
        thread::park();
    }
}
