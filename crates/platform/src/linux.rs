//! Linux writer gate: an exclusive `flock` on a stable, no-follow regular file.

use std::ffi::{CString, OsStr, OsString};
use std::fs::File;
use std::io;
use std::mem::{MaybeUninit, size_of};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::raw::c_int;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use crate::GateError;
use marduk_core::qualification::{Capability, Os, Route, VolumeClass, available};

/// Stable lock object for one application-owned root.
#[derive(Debug)]
pub struct WriterGate {
    path: PathBuf,
    anchor: Option<(File, String)>,
    device: u64,
    inode: u64,
}

/// Held exclusive lock. Closing the file description releases the lock.
#[derive(Debug)]
pub struct WriterGuard {
    _file: File,
}

impl WriterGate {
    /// Open or create the lock file and remember its identity. The parent
    /// directory must already exist and be owned by the application.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, GateError> {
        let path = path.as_ref().to_path_buf();
        let file = open_lock_file(&path, true)?;
        let (device, inode) = identify(&file)?;
        Ok(Self {
            path,
            anchor: None,
            device,
            inode,
        })
    }

    /// Open or create the lock file relative to a pinned directory descriptor.
    /// Reopening for each lock keeps a substituted ancestor out of the path.
    pub fn open_at(parent: &File, name: &str) -> Result<Self, GateError> {
        if !ordinary_component(name) {
            return Err(GateError::Unavailable);
        }
        let anchor = parent.try_clone().map_err(|_| GateError::Io)?;
        let file = open_lock_at(&anchor, name, true)?;
        let (device, inode) = identify(&file)?;
        Ok(Self {
            path: PathBuf::new(),
            anchor: Some((anchor, name.to_owned())),
            device,
            inode,
        })
    }

    /// Take the exclusive lock for this entrant.
    ///
    /// Each entrant opens its own file description: `flock` locks are attached
    /// to the description, so sharing the original descriptor would let two
    /// guards in one process both succeed.
    pub fn try_lock(&self) -> Result<WriterGuard, GateError> {
        let file = match &self.anchor {
            Some((parent, name)) => open_lock_at(parent, name, false)?,
            None => open_lock_file(&self.path, false)?,
        };
        let identity = identify(&file)?;
        if identity != (self.device, self.inode) {
            return Err(GateError::Unavailable);
        }

        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result == 0 {
            return Ok(WriterGuard { _file: file });
        }

        match io::Error::last_os_error().raw_os_error() {
            Some(libc::EWOULDBLOCK) => Err(GateError::Busy),
            _ => Err(GateError::Io),
        }
    }
}

fn open_lock_file(path: &Path, create: bool) -> Result<File, GateError> {
    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| GateError::Unavailable)?;
    let mut flags = libc::O_RDWR | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    if create {
        flags |= libc::O_CREAT;
    }

    let descriptor = unsafe { libc::open(path.as_ptr(), flags, 0o600 as libc::mode_t) };
    if descriptor < 0 {
        return Err(open_error(io::Error::last_os_error()));
    }
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

fn open_lock_at(parent: &File, name: &str, create: bool) -> Result<File, GateError> {
    let name = CString::new(name.as_bytes()).map_err(|_| GateError::Unavailable)?;
    let mut flags = libc::O_RDWR | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    if create {
        flags |= libc::O_CREAT;
    }

    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags,
            0o600 as libc::mode_t,
        )
    };
    if descriptor < 0 {
        return Err(open_error(io::Error::last_os_error()));
    }
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

fn identify(file: &File) -> Result<(u64, u64), GateError> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::fstat(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(GateError::Io);
    }
    let stat = unsafe { stat.assume_init() };

    if stat.st_mode & libc::S_IFMT != libc::S_IFREG || stat.st_nlink != 1 {
        return Err(GateError::Unavailable);
    }
    Ok((stat.st_dev as u64, stat.st_ino as u64))
}

fn open_error(error: io::Error) -> GateError {
    match error.raw_os_error() {
        Some(libc::ELOOP) | Some(libc::ENOENT) | Some(libc::ENOTDIR) => GateError::Unavailable,
        _ => GateError::Io,
    }
}

// ---------------------------------------------------------------------------
// Native root binding and observation.
//
// Every object below the selected installation is opened relative to a verified
// directory descriptor with `openat2` restrictions, then re-verified by
// `fstat`/`statx`/`fstatfs`. `st_dev` is live boundary evidence only. The
// mount ID is also part of the saved token: another view can expose the
// same filesystem, inode and birth time.
// ---------------------------------------------------------------------------

use marduk_core::root_identity::{
    FileMetadata, Fingerprint, GameLayout, NativeIdentity, RootError, RootIdentity,
    TargetObservation, UnsupportedReason,
};
use marduk_core::targets::RelativePath;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::os::fd::RawFd;

const BTRFS_SUPER_MAGIC: u64 = 0x9123_683e;
const TMPFS_SUPER_MAGIC: u64 = 0x0102_1994;
const EXT4_SUPER_MAGIC: u64 = 0xEF53;
const PROC_SUPER_MAGIC: u64 = 0x9fa0;
const BTRFS_FIRST_FREE_OBJECTID: u64 = 256;
/// `_IOR('f', 1, long)` from `linux/fs.h`.
const FS_IOC_GETFLAGS: libc::c_ulong = 0x8008_6601;
const FS_EXTENT_FL: libc::c_long = 0x0008_0000;

// Extents describe the filesystem's ordinary storage format, not user metadata.
fn flags_are_supported(flags: libc::c_long) -> bool {
    flags & !FS_EXTENT_FL == 0
}
/// `_IOR(0x94, 31, struct btrfs_ioctl_fs_info_args)`; the struct is 1 KiB.
const BTRFS_IOC_FS_INFO: libc::c_ulong = 0x8400_941f;
/// `_IOWR(0x94, 18, struct btrfs_ioctl_ino_lookup_args)`; the struct is 4 KiB.
const BTRFS_IOC_INO_LOOKUP: libc::c_ulong = 0xd000_9412;

const STATX_MASK: libc::c_uint = libc::STATX_BASIC_STATS | libc::STATX_BTIME | libc::STATX_MNT_ID;

const NO_LINKS: u64 =
    libc::RESOLVE_BENEATH | libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS;
const OPEN_RESOLVE: u64 = NO_LINKS | libc::RESOLVE_NO_XDEV;
const DIRECTORY_FLAGS: u64 =
    (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW) as u64;
const ANCESTOR_FLAGS: u64 =
    (libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW) as u64;

#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

#[repr(C)]
struct BtrfsFsInfo {
    max_id: u64,
    num_devices: u64,
    fsid: [u8; 16],
    nodesize: u32,
    sectorsize: u32,
    clone_alignment: u32,
    csum_type: u16,
    csum_size: u16,
    flags: u64,
    generation: u64,
    metadata_uuid: [u8; 16],
    reserved: [u8; 944],
}

#[repr(C)]
struct BtrfsInoLookup {
    treeid: u64,
    objectid: u64,
    name: [u8; 4080],
}

/// One verified directory handle held by the binding.
struct RootDir {
    file: File,
    dev_major: u32,
    dev_minor: u32,
    mnt_id: u64,
    tree_id: Option<u64>,
    identity: RootIdentity,
}

pub(crate) struct Binding {
    ancestors: DirectoryChain,
    installation_name: OsString,
    installation: RootDir,
    data: RootDir,
    data_name: String,
}

impl Binding {
    pub(crate) fn identities(&self) -> (&RootIdentity, &RootIdentity) {
        (&self.installation.identity, &self.data.identity)
    }

    pub(crate) fn check_roots(&self) -> Result<(), RootError> {
        let parent = self.ancestors.check(NO_LINKS)?;
        check_directory(
            parent,
            &self.installation_name,
            &self.installation.file,
            NO_LINKS,
        )?;
        check_directory(
            &self.installation.file,
            self.data_name.as_ref(),
            &self.data.file,
            OPEN_RESOLVE,
        )
    }
}

// Each name is relative to the previous pinned descriptor. Linux permits
// renaming open directories, so a held descriptor alone is not chain evidence.
struct DirectoryChain {
    anchor: File,
    children: Vec<(OsString, File)>,
}

impl DirectoryChain {
    fn new(anchor: File) -> Self {
        Self {
            anchor,
            children: Vec::new(),
        }
    }

    fn parent(&self) -> &File {
        self.children.last().map_or(&self.anchor, |(_, file)| file)
    }

    fn check(&self, resolve: u64) -> Result<&File, RootError> {
        let mut parent = &self.anchor;
        for (name, file) in &self.children {
            check_directory(parent, name, file, resolve)?;
            parent = file;
        }
        Ok(parent)
    }
}

fn check_directory(
    parent: &File,
    name: &OsStr,
    file: &File,
    resolve: u64,
) -> Result<(), RootError> {
    let current = if name.is_empty() {
        parent.try_clone().map_err(|_| RootError::Io)?
    } else {
        openat2(parent.as_raw_fd(), name, ANCESTOR_FLAGS, resolve)
            .map_err(|_| RootError::TargetUnavailable)?
    };
    let (current, pinned) = (statx_fd(&current)?, statx_fd(file)?);
    if !same_inode(&current, &pinned) || pinned.stx_nlink == 0 {
        return Err(RootError::TargetUnavailable);
    }
    Ok(())
}

pub(crate) struct ResolvedTarget<'a> {
    parents: DirectoryChain,
    parent_identity: RootIdentity,
    leaf: String,
    logical_leaf: String,
    binding: &'a Binding,
}

impl ResolvedTarget<'_> {
    pub(crate) fn check_chain(&self) -> Result<(), RootError> {
        self.binding.check_roots()?;
        self.parents.check(OPEN_RESOLVE)?;
        Ok(())
    }

    /// A fresh descriptor for the verified parent. It is used directory-relative
    /// by the proof's rename calls, never as standalone authority.
    pub(crate) fn parent_dir(&self) -> Result<File, RootError> {
        self.parents.parent().try_clone().map_err(|_| RootError::Io)
    }

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
    let binding = bind_pair(selection, layout)?;
    if !has_marker(&binding.installation.file, layout)? {
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
    // A reopen never re-runs the layout-marker admission and never adopts a
    // substitute: any failure to reproduce the exact recorded pair is
    // `TargetUnavailable`.
    let reopened = bind_pair(location, layout).map_err(|_| RootError::TargetUnavailable)?;
    if reopened.installation.identity != *installation || reopened.data.identity != *data {
        return Err(RootError::TargetUnavailable);
    }
    Ok(reopened)
}

fn bind_pair(location: &Path, layout: &GameLayout) -> Result<Binding, RootError> {
    let (ancestors, installation_name, installation_file) =
        open_absolute(location, DIRECTORY_FLAGS)?;
    let installation = verify_root(installation_file)?;
    let unsupported = RootError::UnsupportedTarget(UnsupportedReason::Configuration);
    if !ordinary_component(layout.data) {
        return Err(unsupported);
    }
    let data_name = single_match(&installation.file, layout.data).map_err(|_| unsupported)?;
    let data_file = open_component(&installation.file, &data_name).map_err(|_| unsupported)?;
    let data = verify_root(data_file).map_err(|_| unsupported)?;
    if !same_domain(&installation, &data) {
        return Err(unsupported);
    }
    let binding = Binding {
        ancestors,
        installation_name,
        installation,
        data,
        data_name,
    };
    binding.check_roots()?;
    Ok(binding)
}

pub(crate) fn resolve_existing_parent<'a>(
    binding: &'a Binding,
    path: &RelativePath,
) -> Result<ResolvedTarget<'a>, RootError> {
    let mut components: Vec<&str> = path.as_str().split('/').collect();
    let leaf_planned = components.pop().expect("validated path has a component");

    let mut parents =
        DirectoryChain::new(binding.data.file.try_clone().map_err(|_| RootError::Io)?);
    let mut parent_identity = binding.data.identity.clone();

    for component in components {
        let parent = parents.parent();
        let name = single_match(parent, component)?;
        reject_link(parent, &name)?;
        let opened = open_component(parent, &name)?;
        let stx = statx_fd(&opened)?;
        if stx.stx_mode as u32 & libc::S_IFMT != libc::S_IFDIR {
            return Err(RootError::UnsupportedTarget(UnsupportedReason::ObjectKind));
        }
        ensure_within(&binding.data, &opened, &stx)?;

        parent_identity = RootIdentity::from_adapter_evidence(identity_from(&opened, &stx)?);
        parents.children.push((name.into(), opened));
    }

    let parent = parents.parent();
    let leaf = match leaf_matches(parent, leaf_planned)? {
        Some(name) => {
            reject_link(parent, &name)?;
            name
        }
        None => leaf_planned.to_owned(),
    };

    let target = ResolvedTarget {
        parents,
        parent_identity,
        leaf,
        logical_leaf: leaf_planned.to_owned(),
        binding,
    };
    target.check_chain()?;
    Ok(target)
}

pub(crate) fn observe_target(target: &ResolvedTarget<'_>) -> Result<TargetObservation, RootError> {
    target.check_chain()?;
    let parent = target.parents.parent();
    let Some(name) = leaf_matches(parent, &target.logical_leaf)? else {
        target.check_chain()?;
        return Ok(TargetObservation::Absent);
    };
    let pinned = open_leaf(parent.as_raw_fd(), &name)?;
    let stx = statx_fd(&pinned)?;

    if stx.stx_mask & libc::STATX_TYPE == 0 || stx.stx_mode as u32 & libc::S_IFMT != libc::S_IFREG {
        return Ok(TargetObservation::Unsupported {
            reason: UnsupportedReason::ObjectKind,
        });
    }
    if stx.stx_mask & libc::STATX_BASIC_STATS != libc::STATX_BASIC_STATS
        || stx.stx_nlink != 1
        || stx.stx_uid != unsafe { libc::geteuid() } as u32
        || !group_is_current(stx.stx_gid)
        || stx.stx_mode as u32 & 0o7000 != 0
    {
        return Ok(TargetObservation::Unsupported {
            reason: UnsupportedReason::Metadata,
        });
    }

    let Ok(file) = reopen_pinned_file(&pinned) else {
        return Ok(TargetObservation::Unsupported {
            reason: UnsupportedReason::Metadata,
        });
    };
    let before = statx_fd(&file)?;
    if !same_object(&stx, &before) {
        return Err(RootError::Io);
    }
    ensure_within(&target.binding.data, &file, &before)?;

    let fingerprint = match fingerprint_open_file(&file) {
        Ok(fingerprint) => fingerprint,
        Err(RootError::UnsupportedTarget(reason)) => {
            return Ok(TargetObservation::Unsupported { reason });
        }
        Err(error) => return Err(error),
    };
    // The chain and the public leaf are checked again after the read so the
    // fingerprint still belongs to the authorized name.
    target.check_chain()?;
    let name = leaf_matches(parent, &target.logical_leaf)?.ok_or(RootError::TargetUnavailable)?;
    let current = statx_at(parent.as_raw_fd(), &name, libc::AT_SYMLINK_NOFOLLOW)
        .map_err(|_| RootError::TargetUnavailable)?;
    let after = statx_fd(&file)?;
    if !same_object(&after, &current) {
        return Err(RootError::TargetUnavailable);
    }

    Ok(TargetObservation::Regular { fingerprint })
}

fn open_absolute(
    path: &Path,
    final_flags: u64,
) -> Result<(DirectoryChain, OsString, File), RootError> {
    let path = std::path::absolute(path).map_err(|_| RootError::TargetUnavailable)?;
    let anchor = File::open("/").map_err(|_| RootError::Io)?;
    let mut chain = DirectoryChain::new(anchor);
    let mut components = path.components().skip(1).peekable();
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            return Err(RootError::TargetUnavailable);
        };
        let flags = if components.peek().is_none() {
            final_flags
        } else {
            ANCESTOR_FLAGS
        };
        let child = openat2(chain.parent().as_raw_fd(), name, flags, NO_LINKS)
            .map_err(|_| RootError::TargetUnavailable)?;
        chain.children.push((name.to_owned(), child));
    }
    let (name, file) = if let Some(last) = chain.children.pop() {
        last
    } else {
        (
            OsString::new(),
            chain.anchor.try_clone().map_err(|_| RootError::Io)?,
        )
    };
    Ok((chain, name, file))
}

fn open_component(parent: &File, name: &str) -> Result<File, RootError> {
    openat2(
        parent.as_raw_fd(),
        name.as_ref(),
        DIRECTORY_FLAGS,
        OPEN_RESOLVE,
    )
    .map_err(|error| match error.raw_os_error() {
        Some(libc::ENOENT) | Some(libc::ENOTDIR) => RootError::MissingParent,
        Some(libc::ELOOP) | Some(libc::EXDEV) => RootError::OutsideBoundary,
        _ => RootError::Io,
    })
}

fn open_leaf(parent_fd: RawFd, name: &str) -> Result<File, RootError> {
    openat2(
        parent_fd,
        name.as_ref(),
        (libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW) as u64,
        OPEN_RESOLVE,
    )
    .map_err(|error| match error.raw_os_error() {
        Some(libc::ENOENT) => RootError::TargetUnavailable,
        Some(libc::ELOOP) | Some(libc::EXDEV) => RootError::OutsideBoundary,
        _ => RootError::Io,
    })
}

fn reopen_pinned_file(pinned: &File) -> Result<File, RootError> {
    // O_PATH cannot perform the metadata ioctls. Reopen the already checked
    // inode via kernel procfs, never a second lookup of the mod's filename.
    let (_chain, _, proc) = open_absolute(Path::new("/proc"), ANCESTOR_FLAGS)?;
    if statfs_of(&proc)?.0 != PROC_SUPER_MAGIC {
        return Err(RootError::TargetUnavailable);
    }
    // Resolve `self` using the PID namespace of this procfs mount.
    let mut pid = [0u8; 32];
    let len = unsafe {
        libc::readlinkat(
            proc.as_raw_fd(),
            c"self".as_ptr(),
            pid.as_mut_ptr().cast(),
            pid.len(),
        )
    };
    if len <= 0 || len as usize == pid.len() || !pid[..len as usize].iter().all(u8::is_ascii_digit)
    {
        return Err(RootError::TargetUnavailable);
    }
    let process = openat2(
        proc.as_raw_fd(),
        OsStr::from_bytes(&pid[..len as usize]),
        ANCESTOR_FLAGS,
        OPEN_RESOLVE,
    )
    .map_err(|_| RootError::TargetUnavailable)?;
    let directory = openat2(
        process.as_raw_fd(),
        "fd".as_ref(),
        ANCESTOR_FLAGS,
        OPEN_RESOLVE,
    )
    .map_err(|_| RootError::TargetUnavailable)?;
    let name = CString::new(pinned.as_raw_fd().to_string()).map_err(|_| RootError::Io)?;
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if descriptor < 0 {
        return Err(RootError::Io);
    }
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

fn openat2(dirfd: RawFd, name: &OsStr, flags: u64, resolve: u64) -> io::Result<File> {
    openat2_mode(dirfd, name, flags, 0, resolve)
}

fn openat2_mode(
    dirfd: RawFd,
    name: &OsStr,
    flags: u64,
    mode: u64,
    resolve: u64,
) -> io::Result<File> {
    let name =
        CString::new(name.as_bytes()).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    let how = OpenHow {
        flags,
        mode,
        resolve,
    };
    let descriptor = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            dirfd,
            name.as_ptr(),
            &how as *const OpenHow,
            size_of::<OpenHow>(),
        ) as c_int
    };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

fn verify_root(file: File) -> Result<RootDir, RootError> {
    let stx = statx_fd(&file)?;
    if stx.stx_mode as u32 & libc::S_IFMT != libc::S_IFDIR {
        return Err(RootError::TargetUnavailable);
    }
    let boundary = mount_id(&stx)?;
    let identity = RootIdentity::from_adapter_evidence(identity_from(&file, &stx)?);
    let tree_id = tree_id_of(&file)?;
    Ok(RootDir {
        file,
        dev_major: stx.stx_dev_major,
        dev_minor: stx.stx_dev_minor,
        mnt_id: boundary,
        tree_id,
        identity,
    })
}

fn statx_fd(file: &File) -> Result<libc::statx, RootError> {
    statx_fd_io(file).map_err(|_| RootError::Io)
}

fn statx_fd_io(file: &File) -> io::Result<libc::statx> {
    let mut stx = MaybeUninit::<libc::statx>::zeroed();
    let result = unsafe {
        libc::statx(
            file.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH,
            STATX_MASK,
            stx.as_mut_ptr(),
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { stx.assume_init() })
}

fn statx_at(dirfd: RawFd, name: &str, flags: c_int) -> io::Result<libc::statx> {
    let name =
        CString::new(name.as_bytes()).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    let mut stx = MaybeUninit::<libc::statx>::zeroed();
    let result = unsafe { libc::statx(dirfd, name.as_ptr(), flags, STATX_MASK, stx.as_mut_ptr()) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { stx.assume_init() })
}

fn mount_id(stx: &libc::statx) -> Result<u64, RootError> {
    if stx.stx_mask & libc::STATX_MNT_ID == 0 || stx.stx_mnt_id == 0 {
        return Err(RootError::UnsupportedTarget(
            UnsupportedReason::Configuration,
        ));
    }
    Ok(stx.stx_mnt_id)
}

fn identity_from(file: &File, stx: &libc::statx) -> Result<NativeIdentity, RootError> {
    const REQUIRED: u32 = libc::STATX_BTIME | libc::STATX_INO;
    if stx.stx_mask & REQUIRED != REQUIRED || stx.stx_ino == 0 {
        return Err(RootError::UnsupportedTarget(
            UnsupportedReason::Configuration,
        ));
    }
    let (magic, fsid_words) = statfs_of(file)?;
    let (fs_id, tree_id) = match magic {
        BTRFS_SUPER_MAGIC => (btrfs_fsid(file)?, Some(btrfs_tree_id(file)?)),
        _ => (encode_words(fsid_words), None),
    };
    if magic == 0 || fs_id == [0; 16] {
        return Err(RootError::UnsupportedTarget(
            UnsupportedReason::Configuration,
        ));
    }
    Ok(NativeIdentity::Linux {
        fs_magic: magic,
        fs_id,
        view_id: mount_id(stx)?,
        tree_id,
        ino: stx.stx_ino,
        birth: (stx.stx_btime.tv_sec, stx.stx_btime.tv_nsec),
    })
}

fn statfs_of(file: &File) -> Result<(u64, [i32; 2]), RootError> {
    let mut stat = MaybeUninit::<libc::statfs>::zeroed();
    if unsafe { libc::fstatfs(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(RootError::Io);
    }
    let stat = unsafe { stat.assume_init() };
    let magic = stat.f_type as u64;
    // `fsid_t` is two native i32 words on Linux; read them without relying on
    // the private libc field name.
    let words: [i32; 2] =
        unsafe { std::ptr::read_unaligned(std::ptr::addr_of!(stat.f_fsid).cast()) };
    Ok((magic, words))
}

fn encode_words(words: [i32; 2]) -> [u8; 16] {
    let mut encoded = [0u8; 16];
    encoded[..4].copy_from_slice(&words[0].to_le_bytes());
    encoded[4..8].copy_from_slice(&words[1].to_le_bytes());
    encoded
}

fn btrfs_fsid(file: &File) -> Result<[u8; 16], RootError> {
    let mut info = MaybeUninit::<BtrfsFsInfo>::zeroed();
    if unsafe { libc::ioctl(file.as_raw_fd(), BTRFS_IOC_FS_INFO, info.as_mut_ptr()) } != 0 {
        return Err(RootError::UnsupportedTarget(
            UnsupportedReason::Configuration,
        ));
    }
    Ok(unsafe { info.assume_init() }.fsid)
}

fn btrfs_tree_id(file: &File) -> Result<u64, RootError> {
    let mut args = BtrfsInoLookup {
        treeid: 0,
        objectid: BTRFS_FIRST_FREE_OBJECTID,
        name: [0; 4080],
    };
    if unsafe { libc::ioctl(file.as_raw_fd(), BTRFS_IOC_INO_LOOKUP, &mut args) } != 0 {
        return Err(RootError::UnsupportedTarget(
            UnsupportedReason::Configuration,
        ));
    }
    if args.treeid == 0 {
        return Err(RootError::UnsupportedTarget(
            UnsupportedReason::Configuration,
        ));
    }
    Ok(args.treeid)
}

fn tree_id_of(file: &File) -> Result<Option<u64>, RootError> {
    let (magic, _) = statfs_of(file)?;
    if magic == BTRFS_SUPER_MAGIC {
        btrfs_tree_id(file).map(Some)
    } else {
        Ok(None)
    }
}

fn same_domain(left: &RootDir, right: &RootDir) -> bool {
    left.dev_major == right.dev_major
        && left.dev_minor == right.dev_minor
        && left.mnt_id == right.mnt_id
        && left.tree_id == right.tree_id
}

fn ensure_within(root: &RootDir, file: &File, stx: &libc::statx) -> Result<(), RootError> {
    let boundary = mount_id(stx)?;
    if stx.stx_dev_major != root.dev_major
        || stx.stx_dev_minor != root.dev_minor
        || boundary != root.mnt_id
        || tree_id_of(file)? != root.tree_id
    {
        return Err(RootError::OutsideBoundary);
    }
    Ok(())
}

fn same_inode(left: &libc::statx, right: &libc::statx) -> bool {
    const REQUIRED: u32 = libc::STATX_INO | libc::STATX_MNT_ID;
    left.stx_mask & REQUIRED == REQUIRED
        && right.stx_mask & REQUIRED == REQUIRED
        && left.stx_mnt_id == right.stx_mnt_id
        && left.stx_dev_major == right.stx_dev_major
        && left.stx_dev_minor == right.stx_dev_minor
        && left.stx_ino == right.stx_ino
        && left.stx_btime.tv_sec == right.stx_btime.tv_sec
        && left.stx_btime.tv_nsec == right.stx_btime.tv_nsec
}

fn same_object(left: &libc::statx, right: &libc::statx) -> bool {
    const REQUIRED: u32 = STATX_MASK;
    left.stx_mask & REQUIRED == REQUIRED
        && right.stx_mask & REQUIRED == REQUIRED
        && same_inode(left, right)
        && left.stx_mode == right.stx_mode
        && left.stx_nlink == right.stx_nlink
        && left.stx_uid == right.stx_uid
        && left.stx_gid == right.stx_gid
        && left.stx_size == right.stx_size
        && left.stx_mtime.tv_sec == right.stx_mtime.tv_sec
        && left.stx_mtime.tv_nsec == right.stx_mtime.tv_nsec
        && left.stx_ctime.tv_sec == right.stx_ctime.tv_sec
        && left.stx_ctime.tv_nsec == right.stx_ctime.tv_nsec
}

// ---------------------------------------------------------------------------
// Verified private staging tree.
//
// The catalog root being on a qualified volume says nothing about the tree the
// stage will write. Bind `revisions/` once by a no-follow handle, prove it is a
// real directory owned by this user on the same dev/mount/btrfs tree, and keep
// the handle live. Revisions are created and reopened relative to that handle.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
struct StagingDomain {
    dev_major: u32,
    dev_minor: u32,
    mnt_id: u64,
    tree_id: Option<u64>,
}

pub(crate) struct StagedDirectory {
    file: File,
    identity: RootIdentity,
    mode: u32,
    uid: u32,
    gid: u32,
    domain: StagingDomain,
}

impl StagedDirectory {
    pub(crate) fn identity(&self) -> &RootIdentity {
        &self.identity
    }

    /// Open a verified component of this directory. Every component is resolved
    /// beneath the held handle with symlinks and cross-device traversal refused.
    fn open_dir_at(&self, relative: &[&str]) -> io::Result<File> {
        let mut current = self.file.try_clone()?;
        for component in checked_components(relative)? {
            current = openat2(
                current.as_raw_fd(),
                component.as_ref(),
                DIRECTORY_FLAGS,
                OPEN_RESOLVE,
            )?;
        }
        Ok(current)
    }

    /// Create missing private directory components relative to the held handle.
    /// An existing component is reopened without following a link; the owner is
    /// checked so a pre-created substitute cannot redirect later writes.
    ///
    /// The check proves the component is this user's directory, not that it is
    /// on the revision's filesystem domain. A same-owner nested Btrfs subvolume
    /// planted under `payload/` passes here and below; only the revision
    /// directory itself re-proves its Btrfs tree id.
    fn ensure_dir_at(&self, relative: &[&str]) -> io::Result<File> {
        let mut current = self.file.try_clone()?;
        for component in checked_components(relative)? {
            let name =
                CString::new(component).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
            if unsafe { libc::mkdirat(current.as_raw_fd(), name.as_ptr(), 0o700 as libc::mode_t) }
                != 0
            {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::EEXIST) {
                    return Err(error);
                }
            }
            let next = openat2(
                current.as_raw_fd(),
                component.as_ref(),
                DIRECTORY_FLAGS,
                OPEN_RESOLVE,
            )?;
            let stx = statx_fd_io(&next)?;
            if stx.stx_uid != unsafe { libc::geteuid() } as u32 {
                return Err(io::Error::from_raw_os_error(libc::EACCES));
            }
            current = next;
        }
        Ok(current)
    }

    pub(crate) fn create_private_file(&self, relative: &[&str]) -> io::Result<File> {
        let (leaf, parents) = relative
            .split_last()
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
        let directory = self.ensure_dir_at(parents)?;
        openat2_mode(
            directory.as_raw_fd(),
            leaf.as_ref(),
            (libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW)
                as u64,
            0o600,
            OPEN_RESOLVE,
        )
    }

    pub(crate) fn open_relative_file(&self, relative: &[&str]) -> io::Result<File> {
        let (leaf, parents) = relative
            .split_last()
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
        let directory = self.open_dir_at(parents)?;
        openat2(
            directory.as_raw_fd(),
            leaf.as_ref(),
            (libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW) as u64,
            OPEN_RESOLVE,
        )
    }

    /// List one directory relative to the held handle. The returned booleans are
    /// `is_file` and `is_dir`; a third object kind is reported as neither.
    pub(crate) fn list_relative(&self, relative: &[&str]) -> io::Result<Vec<(String, bool, bool)>> {
        let directory = self.open_dir_at(relative)?;
        let mut entries = Vec::new();
        for name in read_directory_io(&directory)? {
            let stx = statx_at(directory.as_raw_fd(), &name, libc::AT_SYMLINK_NOFOLLOW)?;
            let kind = stx.stx_mode as u32 & libc::S_IFMT;
            entries.push((name, kind == libc::S_IFREG, kind == libc::S_IFDIR));
        }
        Ok(entries)
    }

    pub(crate) fn remove_relative_file(&self, relative: &[&str]) -> io::Result<()> {
        let (leaf, parents) = relative
            .split_last()
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
        let directory = self.open_dir_at(parents)?;
        let name = CString::new(*leaf).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
        if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ENOENT) {
                return Err(error);
            }
        }
        Ok(())
    }

    /// Remove an empty directory relative to the held handle. `Ok(false)` means
    /// the directory still had entries; `Ok(true)` means it was removed or was
    /// already absent.
    pub(crate) fn remove_relative_dir(&self, relative: &[&str]) -> io::Result<bool> {
        let (leaf, parents) = relative
            .split_last()
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
        let directory = self.open_dir_at(parents)?;
        let target = match openat2(
            directory.as_raw_fd(),
            leaf.as_ref(),
            DIRECTORY_FLAGS,
            OPEN_RESOLVE,
        ) {
            Ok(target) => target,
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => return Ok(true),
            Err(error) => return Err(error),
        };
        if !read_directory_io(&target)?.is_empty() {
            return Ok(false);
        }
        let name = CString::new(*leaf).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
        if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } != 0
        {
            let error = io::Error::last_os_error();
            return match error.raw_os_error() {
                Some(libc::ENOENT) => Ok(true),
                Some(libc::ENOTEMPTY) => Ok(false),
                _ => Err(error),
            };
        }
        Ok(true)
    }

    pub(crate) fn sync_relative_dir(&self, relative: &[&str]) -> io::Result<()> {
        self.open_dir_at(relative)?.sync_all()
    }

    fn open_child(parent: &RootDir, name: &str) -> Result<Self, RootError> {
        let file = openat2(
            parent.file.as_raw_fd(),
            name.as_ref(),
            DIRECTORY_FLAGS,
            OPEN_RESOLVE,
        )
        .map_err(open_child_error)?;
        let stx = statx_fd(&file)?;
        Self::from_verified(parent, file, &stx)
    }

    fn from_verified(root: &RootDir, file: File, stx: &libc::statx) -> Result<Self, RootError> {
        let mode = stx.stx_mode as u32;
        if mode & libc::S_IFMT != libc::S_IFDIR {
            return Err(RootError::TargetUnavailable);
        }
        ensure_within(root, &file, stx)?;
        let uid = unsafe { libc::geteuid() } as u32;
        if stx.stx_uid != uid {
            return Err(RootError::OutsideBoundary);
        }
        let identity = RootIdentity::from_adapter_evidence(identity_from(&file, stx)?);
        let tree_id = tree_id_of(&file)?;
        Ok(Self {
            file,
            identity,
            mode,
            uid,
            gid: stx.stx_gid,
            domain: StagingDomain {
                dev_major: stx.stx_dev_major,
                dev_minor: stx.stx_dev_minor,
                mnt_id: mount_id(stx)?,
                tree_id,
            },
        })
    }

    /// Reobserve the live handle: identity, object type, owner, mode and the
    /// same filesystem/mount/subvolume boundary as the verified root.
    fn verify(&self, root: &RootDir) -> Result<(), RootError> {
        let stx = statx_fd(&self.file)?;
        let mode = stx.stx_mode as u32;
        if mode & libc::S_IFMT != libc::S_IFDIR {
            return Err(RootError::TargetUnavailable);
        }
        ensure_within(root, &self.file, &stx)?;
        let current = StagingDomain {
            dev_major: stx.stx_dev_major,
            dev_minor: stx.stx_dev_minor,
            mnt_id: mount_id(&stx)?,
            tree_id: tree_id_of(&self.file)?,
        };
        let identity = RootIdentity::from_adapter_evidence(identity_from(&self.file, &stx)?);
        if current != self.domain
            || identity != self.identity
            || mode != self.mode
            || stx.stx_uid != self.uid
            || stx.stx_gid != self.gid
            || self.uid != unsafe { libc::geteuid() } as u32
        {
            return Err(RootError::TargetUnavailable);
        }
        Ok(())
    }
}

pub(crate) struct StagingTree {
    root: RootDir,
    root_path: PathBuf,
    revisions: StagedDirectory,
}

impl StagingTree {
    /// The pinned root handle, for the evidence-only catalog composition that
    /// must not open the same root a second time.
    #[cfg(feature = "evidence")]
    pub(crate) fn root_file(&self) -> &std::fs::File {
        &self.root.file
    }

    pub(crate) fn open(root: &Path) -> Result<Self, RootError> {
        let root_dir = verify_root(open_directory(root)?)?;
        if !library_filesystem_qualified(statfs_of(&root_dir.file)?.0) {
            return Err(RootError::UnsupportedTarget(
                UnsupportedReason::Configuration,
            ));
        }
        let revisions = StagedDirectory::open_child(&root_dir, "revisions")?;
        let tree = Self {
            root: root_dir,
            root_path: root.to_path_buf(),
            revisions,
        };
        tree.reverify_revisions()?;
        Ok(tree)
    }

    pub(crate) fn reverify_revisions(&self) -> Result<(), RootError> {
        self.revisions.verify(&self.root)?;
        let path = open_directory(&self.root_path.join("revisions"))?;
        let stx = statx_fd(&path)?;
        let identity = RootIdentity::from_adapter_evidence(identity_from(&path, &stx)?);
        if identity != self.revisions.identity {
            return Err(RootError::TargetUnavailable);
        }
        Ok(())
    }

    pub(crate) fn create_revision(&self, name: &str) -> Result<StagedDirectory, RootError> {
        self.reverify_revisions()?;
        let name_c = CString::new(name.as_bytes())
            .map_err(|_| RootError::UnsupportedTarget(UnsupportedReason::Configuration))?;
        let created = unsafe {
            libc::mkdirat(
                self.revisions.file.as_raw_fd(),
                name_c.as_ptr(),
                0o700 as libc::mode_t,
            )
        };
        if created != 0 {
            return Err(open_child_error(io::Error::last_os_error()));
        }
        let file = openat2(
            self.revisions.file.as_raw_fd(),
            name.as_ref(),
            DIRECTORY_FLAGS,
            OPEN_RESOLVE,
        )
        .map_err(open_child_error)?;
        let stx = statx_fd(&file)?;
        let revision = StagedDirectory::from_verified(&self.root, file, &stx)?;
        if revision.mode & 0o777 != 0o700 {
            return Err(RootError::TargetUnavailable);
        }
        self.verify_revision_path(name, &revision)?;
        Ok(revision)
    }

    pub(crate) fn open_revision(&self, name: &str) -> Result<Option<StagedDirectory>, RootError> {
        let file = match openat2(
            self.revisions.file.as_raw_fd(),
            name.as_ref(),
            DIRECTORY_FLAGS,
            OPEN_RESOLVE,
        ) {
            Ok(file) => file,
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => return Ok(None),
            Err(error) => return Err(open_child_error(error)),
        };
        let stx = statx_fd(&file)?;
        let revision = StagedDirectory::from_verified(&self.root, file, &stx)?;
        if revision.mode & 0o777 != 0o700 {
            return Err(RootError::TargetUnavailable);
        }
        Ok(Some(revision))
    }

    /// Reopen the exact revision relative to the verified handle and prove the
    /// path still names the same object.
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
        revision.verify(&self.root)?;
        self.reverify_revisions()?;
        self.verify_revision_path(name, revision)
    }

    pub(crate) fn sync_revisions(&self) -> Result<(), RootError> {
        self.revisions.file.sync_all().map_err(|_| RootError::Io)
    }

    /// Flush the bound root handle rather than reopening the root pathname.
    pub(crate) fn sync_root(&self) -> Result<(), RootError> {
        self.root.file.sync_all().map_err(|_| RootError::Io)
    }

    /// Remove the name from the bound `revisions/` handle. `Ok(false)` means the
    /// directory was not empty; `Ok(true)` means removed or already absent.
    pub(crate) fn remove_revision_dir(&self, name: &str) -> io::Result<bool> {
        if !ordinary_component(name) {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        }
        let directory = match openat2(
            self.revisions.file.as_raw_fd(),
            name.as_ref(),
            DIRECTORY_FLAGS,
            OPEN_RESOLVE,
        ) {
            Ok(directory) => directory,
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => return Ok(true),
            Err(error) => return Err(error),
        };
        if !read_directory_io(&directory)?.is_empty() {
            return Ok(false);
        }
        let name = CString::new(name).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
        if unsafe {
            libc::unlinkat(
                self.revisions.file.as_raw_fd(),
                name.as_ptr(),
                libc::AT_REMOVEDIR,
            )
        } != 0
        {
            let error = io::Error::last_os_error();
            return match error.raw_os_error() {
                Some(libc::ENOENT) => Ok(true),
                Some(libc::ENOTEMPTY) => Ok(false),
                _ => Err(error),
            };
        }
        Ok(true)
    }

    fn verify_revision_path(
        &self,
        name: &str,
        revision: &StagedDirectory,
    ) -> Result<(), RootError> {
        let path = open_directory(&self.root_path.join("revisions").join(name))?;
        let stx = statx_fd(&path)?;
        let identity = RootIdentity::from_adapter_evidence(identity_from(&path, &stx)?);
        if identity != revision.identity {
            return Err(RootError::TargetUnavailable);
        }
        Ok(())
    }
}

fn open_child_error(error: io::Error) -> RootError {
    match error.raw_os_error() {
        Some(libc::ENOENT) | Some(libc::ENOTDIR) => RootError::MissingParent,
        Some(libc::ELOOP) | Some(libc::EXDEV) | Some(libc::EEXIST) => RootError::TargetUnavailable,
        _ => RootError::Io,
    }
}

fn supported_metadata(file: &File) -> bool {
    let mut flags: libc::c_long = 0;
    unsafe {
        libc::ioctl(file.as_raw_fd(), FS_IOC_GETFLAGS, &mut flags) == 0
            && flags_are_supported(flags)
            && libc::flistxattr(file.as_raw_fd(), std::ptr::null_mut(), 0) == 0
    }
}

/// Fingerprint an already open regular file. The caller owns the boundary
/// decision; this helper only proves the object's own shape, metadata, bytes
/// and stability across the read.
pub(crate) fn fingerprint_open_file(file: &File) -> Result<Fingerprint, RootError> {
    let before = statx_fd(file)?;
    if before.stx_mode as u32 & libc::S_IFMT != libc::S_IFREG {
        return Err(RootError::UnsupportedTarget(UnsupportedReason::ObjectKind));
    }
    if before.stx_mask & libc::STATX_BASIC_STATS != libc::STATX_BASIC_STATS
        || before.stx_nlink != 1
        || before.stx_uid != unsafe { libc::geteuid() } as u32
        || !group_is_current(before.stx_gid)
        || before.stx_mode as u32 & 0o7000 != 0
        || !supported_metadata(file)
    {
        return Err(RootError::UnsupportedTarget(UnsupportedReason::Metadata));
    }

    let identity = identity_from(file, &before)?;
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
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(RootError::Io),
        }
    }
    let after = statx_fd(file)?;
    if !same_object(&before, &after) || len != before.stx_size || !supported_metadata(file) {
        return Err(RootError::Io);
    }

    let mut sha256 = [0u8; 32];
    sha256.copy_from_slice(&hasher.finalize());
    Ok(Fingerprint {
        identity,
        len,
        sha256,
        metadata: FileMetadata::Linux {
            mode: before.stx_mode as u32,
            uid: before.stx_uid,
            gid: before.stx_gid,
            mtime: (before.stx_mtime.tv_sec, before.stx_mtime.tv_nsec),
            nlink: u64::from(before.stx_nlink),
        },
    })
}

// ---------------------------------------------------------------------------
// Synthetic proof support. These helpers expose no new authority: callers
// still re-verify the live binding before every effect.
// ---------------------------------------------------------------------------

pub(crate) fn directory_identity(path: &Path) -> Result<RootIdentity, RootError> {
    let (_chain, _name, file) = open_absolute(path, DIRECTORY_FLAGS)?;
    let stx = statx_fd(&file)?;
    if stx.stx_mode as u32 & libc::S_IFMT != libc::S_IFDIR {
        return Err(RootError::TargetUnavailable);
    }
    Ok(RootIdentity::from_adapter_evidence(identity_from(
        &file, &stx,
    )?))
}

pub(crate) fn open_private_directory(path: &Path) -> Result<File, RootError> {
    let path = std::path::absolute(path).map_err(|_| RootError::TargetUnavailable)?;
    let mut current = File::open("/").map_err(|_| RootError::Io)?;
    for component in path.components().skip(1) {
        let Component::Normal(name) = component else {
            return Err(RootError::TargetUnavailable);
        };
        current = open_or_create_directory(&current, name)?;
    }
    Ok(current)
}

/// POSIX has no per-handle sharing mode, so the evidence open is the same
/// boundary proof as production.
#[cfg(feature = "evidence")]
pub(crate) fn open_private_directory_for_evidence(path: &Path) -> Result<File, RootError> {
    open_private_directory(path)
}

pub(crate) fn open_private_child_directory(parent: &File, name: &str) -> Result<File, RootError> {
    if !ordinary_component(name) {
        return Err(RootError::TargetUnavailable);
    }
    open_or_create_directory(parent, OsStr::new(name))
}

#[cfg(feature = "evidence")]
pub(crate) fn open_private_child_directory_for_evidence(
    parent: &File,
    name: &str,
) -> Result<File, RootError> {
    open_private_child_directory(parent, name)
}

pub(crate) fn private_child_path(parent: &File, name: &str) -> Result<PathBuf, RootError> {
    if !ordinary_component(name) {
        return Err(RootError::TargetUnavailable);
    }
    // SQLite rejects a pathname with any symlink component, so translate the
    // pinned descriptor back to its resolved path. `verify_private_directory`
    // re-proves this spelling around every pathname-based open.
    let proc_path = PathBuf::from(format!("/proc/self/fd/{}", parent.as_raw_fd()));
    let resolved = std::fs::read_link(proc_path).map_err(|_| RootError::TargetUnavailable)?;
    if !resolved.is_absolute() || resolved.to_string_lossy().contains(" (deleted)") {
        return Err(RootError::TargetUnavailable);
    }
    Ok(resolved.join(name))
}

pub(crate) fn verify_private_directory(path: &Path, handle: &File) -> Result<(), RootError> {
    let expected = directory_identity(path)?;
    let stx = statx_fd(handle)?;
    let actual = RootIdentity::from_adapter_evidence(identity_from(handle, &stx)?);
    if expected != actual {
        return Err(RootError::TargetUnavailable);
    }
    Ok(())
}

/// POSIX has no sharing modes, so the evidence check is the same boundary proof.
#[cfg(feature = "evidence")]
pub(crate) fn verify_private_directory_for_evidence(
    path: &Path,
    handle: &File,
) -> Result<(), RootError> {
    verify_private_directory(path, handle)
}

pub(crate) struct PrivateFile {
    file: File,
    name: String,
}

impl PrivateFile {
    pub(crate) fn path(&self, parent: &File) -> Result<PathBuf, RootError> {
        // `/proc/self/fd/<dirfd>` resolves through the pinned directory inode, so
        // renaming the verified directory cannot redirect this path. SQLite
        // resolves the leaf and sidecars itself; the substitution residual is
        // owned by `docs/contracts/m1-recovery.md` (catalog file containment).
        Ok(PathBuf::from(format!(
            "/proc/self/fd/{}/{}",
            parent.as_raw_fd(),
            self.name
        )))
    }

    pub(crate) fn verify(&self, parent: &File) -> Result<(), RootError> {
        let reopened = open_private_file_at(parent, &self.name)?;
        let pinned = statx_fd(&self.file)?;
        let current = statx_fd(&reopened)?;
        let parent_stx = statx_fd(parent)?;
        if !same_inode(&pinned, &current)
            || current.stx_mode as u32 & libc::S_IFMT != libc::S_IFREG
            || current.stx_mnt_id != parent_stx.stx_mnt_id
            || current.stx_dev_major != parent_stx.stx_dev_major
            || current.stx_dev_minor != parent_stx.stx_dev_minor
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
    let file = match open_private_file_at(parent, name) {
        Ok(file) => file,
        Err(RootError::MissingParent) => openat2_mode(
            parent.as_raw_fd(),
            OsStr::new(name),
            (libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW)
                as u64,
            0o600,
            NO_LINKS,
        )
        .map_err(|_| RootError::Io)?,
        Err(error) => return Err(error),
    };
    let stx = statx_fd(&file)?;
    let parent_stx = statx_fd(parent)?;
    if stx.stx_mode as u32 & libc::S_IFMT != libc::S_IFREG
        || stx.stx_mnt_id != parent_stx.stx_mnt_id
        || stx.stx_dev_major != parent_stx.stx_dev_major
        || stx.stx_dev_minor != parent_stx.stx_dev_minor
    {
        return Err(RootError::TargetUnavailable);
    }
    Ok(PrivateFile {
        file,
        name: name.to_owned(),
    })
}

fn open_private_file_at(parent: &File, name: &str) -> Result<File, RootError> {
    openat2(
        parent.as_raw_fd(),
        OsStr::new(name),
        (libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW) as u64,
        NO_LINKS,
    )
    .map_err(|error| match error.raw_os_error() {
        Some(libc::ENOENT) | Some(libc::ENOTDIR) => RootError::MissingParent,
        Some(libc::ELOOP) => RootError::OutsideBoundary,
        _ => RootError::Io,
    })
}

fn open_or_create_directory(parent: &File, name: &OsStr) -> Result<File, RootError> {
    if name.to_str().is_none_or(|name| !ordinary_component(name)) {
        return Err(RootError::TargetUnavailable);
    }
    match openat2(
        parent.as_raw_fd(),
        name,
        DIRECTORY_FLAGS,
        libc::RESOLVE_BENEATH | libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS,
    ) {
        Ok(directory) => Ok(directory),
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {
            let name_c = CString::new(name.as_bytes()).map_err(|_| RootError::TargetUnavailable)?;
            if unsafe { libc::mkdirat(parent.as_raw_fd(), name_c.as_ptr(), 0o700) } != 0 {
                return Err(match io::Error::last_os_error().raw_os_error() {
                    Some(libc::EEXIST) => RootError::TargetUnavailable,
                    _ => RootError::Io,
                });
            }
            openat2(
                parent.as_raw_fd(),
                name,
                DIRECTORY_FLAGS,
                libc::RESOLVE_BENEATH | libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS,
            )
            .map_err(|_| RootError::TargetUnavailable)
        }
        Err(_) => Err(RootError::TargetUnavailable),
    }
}

pub(crate) fn open_directory(path: &Path) -> Result<File, RootError> {
    let (_chain, _name, file) = open_absolute(path, DIRECTORY_FLAGS)?;
    let stx = statx_fd(&file)?;
    if stx.stx_mode as u32 & libc::S_IFMT != libc::S_IFDIR {
        return Err(RootError::TargetUnavailable);
    }
    Ok(file)
}

pub(crate) fn open_file(path: &Path) -> Result<File, RootError> {
    let (_chain, _name, file) = open_absolute(
        path,
        (libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW) as u64,
    )?;
    Ok(file)
}

pub(crate) fn sync_directory(path: &Path) -> Result<(), RootError> {
    open_directory(path)?.sync_all().map_err(|_| RootError::Io)
}

/// Witness-only privacy proof for one staged path: a directory is a real
/// directory with mode `0700`, a file a regular file with mode `0600`, both
/// owned by the effective user.
pub(crate) fn staged_entry_is_private(path: &Path, directory: bool) -> bool {
    let opened = if directory {
        open_directory(path)
    } else {
        open_file(path)
    };
    let Ok(file) = opened else {
        return false;
    };
    let Ok(stx) = statx_fd(&file) else {
        return false;
    };
    let mode = stx.stx_mode as u32;
    let (kind, allowed) = if directory {
        (libc::S_IFDIR, 0o700)
    } else {
        (libc::S_IFREG, 0o600)
    };
    mode & libc::S_IFMT == kind
        && mode & 0o777 == allowed
        && stx.stx_uid == unsafe { libc::geteuid() } as u32
}

pub(crate) fn renameat2(
    from_dir: &File,
    from: &str,
    to_dir: &File,
    to: &str,
    flags: u32,
) -> io::Result<()> {
    let from = CString::new(from).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    let to = CString::new(to).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            from_dir.as_raw_fd(),
            from.as_ptr(),
            to_dir.as_raw_fd(),
            to.as_ptr(),
            flags,
        ) as c_int
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Volume class of the observed root's filesystem. The adapter reports the
/// class only; `marduk_core::qualification` decides what it is good for.
fn volume_class(magic: u64) -> VolumeClass {
    match magic {
        BTRFS_SUPER_MAGIC => VolumeClass::Btrfs,
        TMPFS_SUPER_MAGIC => VolumeClass::Tmpfs,
        EXT4_SUPER_MAGIC => VolumeClass::Ext4,
        _ => VolumeClass::Other,
    }
}

fn observed_volume_class(root: &Path) -> Option<VolumeClass> {
    let file = open_directory(root).ok()?;
    let (magic, _) = statfs_of(&file).ok()?;
    Some(volume_class(magic))
}

/// Whether the availability record proves private-library staging on this
/// filesystem class. Decided by `marduk_core::qualification`; this function only
/// maps the observed magic and asks.
fn library_filesystem_qualified(magic: u64) -> bool {
    available(
        Capability::LibraryStaging,
        Route::Import,
        Os::Linux,
        volume_class(magic),
    )
}

/// Library import is connected only where the record proves a catalog-layer
/// run; the observed root's `fstatfs` magic supplies the class, and an
/// unreadable root is unqualified.
pub(crate) fn library_volume_qualified(root: &Path) -> bool {
    observed_volume_class(root)
        .is_some_and(|class| available(Capability::LibraryStaging, Route::Import, Os::Linux, class))
}

/// The composed native effect adapter runs only where the record proves the
/// adapter layer; platform reports the class and does not decide availability.
pub(crate) fn effects_volume_qualified(root: &Path) -> bool {
    observed_volume_class(root)
        .is_some_and(|class| available(Capability::NativeEffects, Route::Adapter, Os::Linux, class))
}

/// The effective group and every supplementary group can be restored without
/// changing ownership.
fn group_is_current(gid: u32) -> bool {
    if gid == unsafe { libc::getegid() } as u32 {
        return true;
    }
    let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    if count <= 0 {
        return false;
    }
    let mut groups = vec![0 as libc::gid_t; count as usize];
    let read = unsafe { libc::getgroups(count, groups.as_mut_ptr()) };
    read >= 0
        && groups[..read as usize]
            .iter()
            .any(|group| *group as u32 == gid)
}

fn has_marker(directory: &File, layout: &GameLayout) -> Result<bool, RootError> {
    for marker in layout.markers {
        if !ordinary_component(marker) {
            continue;
        }
        let Some(name) = leaf_matches(directory, marker)? else {
            continue;
        };
        match statx_at(directory.as_raw_fd(), &name, libc::AT_SYMLINK_NOFOLLOW) {
            Ok(stx) if stx.stx_mode as u32 & libc::S_IFMT == libc::S_IFREG => return Ok(true),
            Ok(_) => continue,
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(libc::ENOENT) | Some(libc::ENOTDIR) | Some(libc::ELOOP)
                ) =>
            {
                continue;
            }
            Err(_) => return Err(RootError::Io),
        }
    }
    Ok(false)
}

fn ordinary_component(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/') && !name.contains('\0')
}

fn checked_components<'a>(relative: &[&'a str]) -> io::Result<Vec<&'a str>> {
    relative
        .iter()
        .map(|component| {
            if ordinary_component(component) {
                Ok(*component)
            } else {
                Err(io::Error::from_raw_os_error(libc::EINVAL))
            }
        })
        .collect()
}

fn read_directory(directory: &File) -> Result<Vec<String>, RootError> {
    read_directory_io(directory).map_err(|_| RootError::Io)
}

fn read_directory_io(directory: &File) -> io::Result<Vec<String>> {
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            c".".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    let listing = unsafe { libc::fdopendir(descriptor) };
    if listing.is_null() {
        let error = io::Error::last_os_error();
        unsafe { libc::close(descriptor) };
        return Err(error);
    }

    let mut names = Vec::new();
    loop {
        unsafe { *libc::__errno_location() = 0 };
        let entry = unsafe { libc::readdir(listing) };
        if entry.is_null() {
            let code = unsafe { *libc::__errno_location() };
            unsafe { libc::closedir(listing) };
            if code == 0 {
                return Ok(names);
            }
            return Err(io::Error::from_raw_os_error(code));
        }
        let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) };
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        if let Ok(name) = std::str::from_utf8(name.to_bytes()) {
            names.push(name.to_owned());
        }
    }
}

fn single_match(directory: &File, component: &str) -> Result<String, RootError> {
    leaf_matches(directory, component)?.ok_or(RootError::MissingParent)
}

fn leaf_matches(directory: &File, component: &str) -> Result<Option<String>, RootError> {
    let mut matches = read_directory(directory)?
        .into_iter()
        .filter(|name| name.eq_ignore_ascii_case(component));
    let entry = matches.next();
    if matches.next().is_some() {
        return Err(RootError::AmbiguousName);
    }
    let fd = directory.as_raw_fd();
    match statx_at(fd, component, libc::AT_SYMLINK_NOFOLLOW) {
        Ok(native) => {
            let name = entry.as_ref().ok_or(RootError::AmbiguousName)?;
            if name != component {
                let listed = statx_at(fd, name, libc::AT_SYMLINK_NOFOLLOW)
                    .map_err(|_| RootError::TargetUnavailable)?;
                if !same_inode(&native, &listed) {
                    return Err(RootError::AmbiguousName);
                }
            }
        }
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {}
        Err(_) => return Err(RootError::Io),
    }
    Ok(entry)
}

fn reject_link(parent: &File, name: &str) -> Result<(), RootError> {
    let stx = match statx_at(parent.as_raw_fd(), name, libc::AT_SYMLINK_NOFOLLOW) {
        Ok(stx) => stx,
        Err(_) => return Err(RootError::MissingParent),
    };
    if stx.stx_mode as u32 & libc::S_IFMT == libc::S_IFLNK {
        return Err(RootError::OutsideBoundary);
    }
    Ok(())
}

#[cfg(test)]
#[path = "linux_root_tests.rs"]
mod linux_root_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_is_exclusive_across_entrants() {
        let directory = tempfile::tempdir().expect("scratch directory");
        let path = directory.path().join("library.lock");

        let gate = WriterGate::open(&path).expect("open gate");
        let first = gate.try_lock().expect("first lock");
        assert!(matches!(gate.try_lock(), Err(GateError::Busy)));

        let second_gate = WriterGate::open(&path).expect("reopen gate");
        assert!(matches!(second_gate.try_lock(), Err(GateError::Busy)));

        drop(first);
        assert!(second_gate.try_lock().is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn gate_refuses_symlink() {
        let directory = tempfile::tempdir().expect("scratch directory");
        let target = directory.path().join("real.lock");
        std::fs::write(&target, []).expect("target");
        let link = directory.path().join("link.lock");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        assert!(matches!(
            WriterGate::open(&link),
            Err(GateError::Unavailable)
        ));
    }
}
