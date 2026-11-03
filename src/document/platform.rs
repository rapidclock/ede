//! Small Unix filesystem boundary: nonblocking regular-file opens and metadata policy.

use super::Error;
use nix::{libc, sys::statvfs::fstatvfs};
use std::{
    fs::{self, File, Metadata, OpenOptions},
    io,
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};

/// File identity and observed version; contents are additionally compared on save.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Version {
    pub dev: u64,
    pub ino: u64,
    pub len: u64,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub links: u64,
    modified: (i64, i64),
    changed: (i64, i64),
    flags: u32,
}
impl Version {
    /// Capture nanosecond version information without relying on atime.
    pub fn of(metadata: &Metadata) -> Self {
        Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
            len: metadata.len(),
            mode: metadata.mode(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            links: metadata.nlink(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
            flags: flags(metadata),
        }
    }
    /// Whether two observations refer to the same filesystem object.
    pub fn same_object(&self, other: &Self) -> bool {
        self.dev == other.dev && self.ino == other.ino
    }
}
/// macOS file flags are metadata that atomic replacement must not silently drop.
#[cfg(target_os = "macos")]
fn flags(metadata: &Metadata) -> u32 {
    std::os::macos::fs::MetadataExt::st_flags(metadata)
}
/// Linux inode flags are queried separately before replacing an existing target.
#[cfg(target_os = "linux")]
fn flags(_: &Metadata) -> u32 {
    0
}

/// Open without FIFO blocking, then validate descriptor and resolved pathname identity.
pub(super) fn open(path: &Path) -> Result<(File, PathBuf, Version), Error> {
    if !fs::metadata(path)?.is_file() {
        return Err(Error::UnsupportedType);
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(Error::UnsupportedType);
    }
    let version = Version::of(&file.metadata()?);
    let resolved = fs::canonicalize(path)?;
    if Version::of(&fs::metadata(&resolved)?) != version {
        return Err(Error::SourceChanged);
    }
    Ok((file, resolved, version))
}
/// Check available storage before allocating, preserving configured free-space headroom.
pub(super) fn reserve(file: &File, bytes: u64, headroom: u64) -> Result<(), Error> {
    let stat = fstatvfs(file).map_err(io::Error::from)?;
    #[cfg(target_os = "macos")]
    let free = u64::from(stat.blocks_available()).saturating_mul(stat.fragment_size());
    #[cfg(target_os = "linux")]
    let free = stat.blocks_available().saturating_mul(stat.fragment_size());
    if free
        < bytes
            .checked_add(headroom)
            .ok_or(Error::ResourceLimit("disk estimate overflow"))?
    {
        return Err(Error::ResourceLimit("insufficient working disk space"));
    }
    Ok(())
}
/// Attempt a private filesystem clone. Unsupported clones use the portable copy path.
#[cfg(target_os = "macos")]
pub(super) fn clone_file(source: &File, path: &Path) -> Result<Option<File>, Error> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let path_c = CString::new(path.as_os_str().as_bytes()).map_err(|_| Error::InvalidRange)?;
    // SAFETY: source is live, path is NUL terminated, and AT_FDCWD accepts absolute paths.
    let result =
        unsafe { libc::fclonefileat(source.as_raw_fd(), libc::AT_FDCWD, path_c.as_ptr(), 0) };
    if result != 0 {
        return Ok(None);
    }
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(Some(file))
}
/// Linux FICLONE preserves independent inodes; fallback never uses a hard link.
#[cfg(target_os = "linux")]
pub(super) fn clone_file(source: &File, path: &Path) -> Result<Option<File>, Error> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    // SAFETY: both descriptors are live regular files; FICLONE takes the source FD value.
    if unsafe { libc::ioctl(file.as_raw_fd(), libc::FICLONE, source.as_raw_fd()) } == 0 {
        return Ok(Some(file));
    }
    drop(file);
    fs::remove_file(path)?;
    Ok(None)
}
/// Reject metadata we cannot yet preserve, before any destination publication.
pub(super) fn replaceable(file: &File, version: &Version) -> Result<(), Error> {
    if version.links != 1 {
        return Err(Error::SaveAsRequired("multiple hard links"));
    }
    if version.mode & 0o7000 != 0 || version.flags != 0 {
        return Err(Error::SaveAsRequired("special permissions or flags"));
    }
    if unsupported_xattrs(file)? || acl(file)? || inode_flags(file)? {
        return Err(Error::SaveAsRequired(
            "extended attributes, ACLs or filesystem flags",
        ));
    }
    Ok(())
}
/// Check all xattr names without allocating an unbounded list (also detects Linux ACLs).
#[cfg(target_os = "linux")]
fn extended(file: &File) -> Result<bool, Error> {
    #[cfg(target_os = "macos")]
    // SAFETY: null/zero requests only the size; descriptor remains live.
    let size = unsafe { libc::flistxattr(file.as_raw_fd(), std::ptr::null_mut(), 0, 0) };
    #[cfg(target_os = "linux")]
    // SAFETY: null/zero requests only the size; descriptor remains live.
    let size = unsafe { libc::flistxattr(file.as_raw_fd(), std::ptr::null_mut(), 0) };
    if size >= 0 {
        return Ok(size != 0);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ENOTSUP) {
        return Ok(false);
    }
    Err(error.into())
}
/// Linux ACLs are xattrs, already inspected above.
#[cfg(target_os = "linux")]
fn acl(_: &File) -> Result<bool, Error> {
    Ok(false)
}
/// Darwin exposes ACLs separately from xattrs. Use the platform ACL API read-only.
#[cfg(target_os = "macos")]
fn acl(file: &File) -> Result<bool, Error> {
    use std::ffi::{c_int, c_void};
    unsafe extern "C" {
        fn acl_get_fd_np(fd: c_int, kind: c_int) -> *mut c_void;
        fn acl_get_entry(acl: *mut c_void, entry: c_int, out: *mut *mut c_void) -> c_int;
        fn acl_free(acl: *mut c_void) -> c_int;
    }
    // SAFETY: live descriptor; Darwin ACL_TYPE_EXTENDED is 0x100.
    let acl = unsafe { acl_get_fd_np(file.as_raw_fd(), 0x100) };
    if acl.is_null() {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOENT) {
            return Ok(false);
        }
        return Err(error.into());
    }
    let mut entry = std::ptr::null_mut();
    // SAFETY: acl is owned and valid until freed; ACL_FIRST_ENTRY is zero.
    let result = unsafe { acl_get_entry(acl, 0, &mut entry) };
    let error = io::Error::last_os_error();
    // SAFETY: release exactly the ACL returned above, after the last access.
    unsafe {
        acl_free(acl);
    }
    // Darwin returns 0 for success and -1/EINVAL when the ACL has no entries.
    if result == 0 {
        return Ok(true);
    }
    if error.raw_os_error() == Some(libc::EINVAL) {
        return Ok(false);
    }
    Err(error.into())
}
/// Darwin flags are included in Version.
#[cfg(target_os = "macos")]
fn inode_flags(_: &File) -> Result<bool, Error> {
    Ok(false)
}
/// Detect Linux inode flags that atomic replacement would discard.
#[cfg(target_os = "linux")]
fn inode_flags(file: &File) -> Result<bool, Error> {
    let mut flags: libc::c_long = 0;
    // FS_IOC_GETFLAGS encodes a long-sized output, despite commonly returning 32 bits.
    const GETFLAGS: libc::c_ulong =
        0x8000_6601 | ((std::mem::size_of::<libc::c_long>() as libc::c_ulong) << 16);
    // SAFETY: ioctl writes at most one c_long into a valid live local allocation.
    let result = unsafe { libc::ioctl(file.as_raw_fd(), GETFLAGS, &mut flags) };
    if result == 0 {
        // EXTENTS is a storage-layout detail, not user metadata to preserve.
        return Ok(flags & !0x0008_0000 != 0);
    }
    let error = io::Error::last_os_error();
    if matches!(error.raw_os_error(), Some(libc::ENOTTY | libc::EOPNOTSUPP)) {
        return Ok(false);
    }
    Err(error.into())
}
/// Preserve and verify ownership and ordinary mode bits on private save output.
pub(super) fn preserve(original: &File, file: &File, version: &Version) -> Result<(), Error> {
    copy_xattrs(original, file)?;
    let current = Version::of(&file.metadata()?);
    if current.uid != version.uid || current.gid != version.gid {
        // SAFETY: live descriptor and numeric owner/group from the accepted target.
        if unsafe { libc::fchown(file.as_raw_fd(), version.uid, version.gid) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
    }
    file.set_permissions(fs::Permissions::from_mode(version.mode & 0o777))?;
    let current = Version::of(&file.metadata()?);
    if current.uid != version.uid
        || current.gid != version.gid
        || current.mode & 0o777 != version.mode & 0o777
    {
        return Err(Error::SaveAsRequired(
            "unable to preserve ownership or permissions",
        ));
    }
    Ok(())
}

/// Linux ACLs/xattrs require a later preservation adapter; reject before publication.
#[cfg(target_os = "linux")]
fn unsupported_xattrs(file: &File) -> Result<bool, Error> {
    extended(file)
}
/// Darwin xattrs are supported only within a bounded, verified native-copy envelope.
#[cfg(target_os = "macos")]
fn unsupported_xattrs(file: &File) -> Result<bool, Error> {
    xattrs(file).map(|_| false)
}
/// Linux metadata policy currently accepts only files without xattrs.
#[cfg(target_os = "linux")]
fn copy_xattrs(original: &File, _: &File) -> Result<(), Error> {
    if extended(original)? {
        return Err(Error::SaveAsRequired("extended attributes"));
    }
    Ok(())
}
/// Bounded Darwin xattr snapshot. Names and values together may not exceed 64 KiB.
#[cfg(target_os = "macos")]
fn xattrs(file: &File) -> Result<std::collections::BTreeMap<Vec<u8>, Vec<u8>>, Error> {
    use std::ffi::CString;
    const LIMIT: usize = 64 << 10;
    // SAFETY: null/zero requests the size on a live descriptor.
    let length = unsafe { libc::flistxattr(file.as_raw_fd(), std::ptr::null_mut(), 0, 0) };
    if length < 0 {
        return Err(io::Error::last_os_error().into());
    }
    if length as usize > LIMIT {
        return Err(Error::SaveAsRequired("extended metadata exceeds 64 KiB"));
    }
    let mut names = vec![0u8; length as usize];
    // SAFETY: names has exactly the allocation passed to the OS.
    let count =
        unsafe { libc::flistxattr(file.as_raw_fd(), names.as_mut_ptr().cast(), names.len(), 0) };
    if count < 0 {
        return Err(io::Error::last_os_error().into());
    }
    names.truncate(count as usize);
    let mut result = std::collections::BTreeMap::new();
    let mut total = names.len();
    for name in names.split(|&b| b == 0).filter(|name| !name.is_empty()) {
        let c_name = CString::new(name).map_err(|_| Error::CorruptStorage)?;
        // SAFETY: NUL-terminated name and live descriptor; null/zero queries size.
        let size = unsafe {
            libc::fgetxattr(
                file.as_raw_fd(),
                c_name.as_ptr(),
                std::ptr::null_mut(),
                0,
                0,
                0,
            )
        };
        if size < 0 {
            return Err(io::Error::last_os_error().into());
        }
        total = total
            .checked_add(size as usize + 256)
            .ok_or(Error::ResourceLimit("metadata size overflow"))?;
        if total > LIMIT {
            return Err(Error::SaveAsRequired("extended metadata exceeds 64 KiB"));
        }
        let mut value = vec![0u8; size as usize];
        // SAFETY: value allocation and NUL-terminated name remain live throughout the call.
        let read = unsafe {
            libc::fgetxattr(
                file.as_raw_fd(),
                c_name.as_ptr(),
                value.as_mut_ptr().cast(),
                value.len(),
                0,
                0,
            )
        };
        if read < 0 {
            return Err(io::Error::last_os_error().into());
        }
        value.truncate(read as usize);
        result.insert(name.to_vec(), value);
    }
    Ok(result)
}
/// Copy macOS xattrs through the native descriptor API, then compare every name/value.
#[cfg(target_os = "macos")]
fn copy_xattrs(original: &File, output: &File) -> Result<(), Error> {
    unsafe extern "C" {
        fn fcopyfile(
            from: libc::c_int,
            to: libc::c_int,
            state: *mut libc::c_void,
            flags: u32,
        ) -> libc::c_int;
    }
    let before = xattrs(original)?;
    // SAFETY: live descriptors; null state requests internal temporary state; flag 4 is COPYFILE_XATTR only.
    if unsafe {
        fcopyfile(
            original.as_raw_fd(),
            output.as_raw_fd(),
            std::ptr::null_mut(),
            4,
        )
    } != 0
    {
        return Err(io::Error::last_os_error().into());
    }
    if xattrs(output)? != before || xattrs(original)? != before {
        return Err(Error::SaveAsRequired(
            "extended metadata could not be preserved exactly",
        ));
    }
    Ok(())
}
