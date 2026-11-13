use std::borrow::Cow;
use std::ffi::OsString;
use std::fs::{self, DirBuilder, File, FileType, Metadata, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// The raw error a rename across file systems fails with.
pub(crate) const CROSS_DEVICE: i32 = libc::EXDEV;

pub(crate) const BAD_NAME: &str = "The name cannot contain \"/\"";

pub(crate) fn bad_name_char(c: char) -> bool {
    c == '/' || c == '\0'
}

pub(crate) fn dev(m: &Metadata, _path: &Path) -> Option<u64> {
    Some(m.dev())
}

pub(crate) fn same_file(a: &Metadata, _: &Path, b: &Metadata, _: &Path) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}

pub(crate) fn mode(m: &Metadata) -> u32 {
    m.mode()
}

pub(crate) fn owner(m: &Metadata) -> u32 {
    m.uid()
}

pub(crate) fn set_file_mode(f: &File, mode: u32) -> io::Result<()> {
    f.set_permissions(fs::Permissions::from_mode(mode))
}

pub(crate) fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

/// Permission bits for something being created.
pub(crate) trait ModeExt {
    fn unix_mode(&mut self, mode: u32) -> &mut Self;
}

impl ModeExt for OpenOptions {
    fn unix_mode(&mut self, mode: u32) -> &mut Self {
        self.mode(mode)
    }
}

impl ModeExt for DirBuilder {
    fn unix_mode(&mut self, mode: u32) -> &mut Self {
        self.mode(mode)
    }
}

/// Fails the open rather than follow a link at the last component.
pub(crate) fn no_follow(o: &mut OpenOptions) -> &mut OpenOptions {
    o.custom_flags(libc::O_NOFOLLOW)
}

pub(crate) fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// Recreates the link `ft` describes, pointing at `target`.
pub(crate) fn copy_symlink(_ft: &FileType, target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

pub(crate) fn uid() -> u32 {
    // SAFETY: getuid cannot fail.
    unsafe { libc::getuid() }
}

/// Whether chmod(2) on a file owned by `uid` can succeed for this process: it owns the file,
/// or it is root.
pub(crate) fn may_change_mode(uid: u32) -> bool {
    // SAFETY: geteuid has no preconditions and cannot fail.
    let me = unsafe { libc::geteuid() };
    me == 0 || me == uid
}

pub(crate) fn home() -> Option<OsString> {
    std::env::var_os("HOME")
}

/// `tool` as a file name on PATH.
pub(crate) fn exe_name(tool: &str) -> Cow<'_, str> {
    Cow::Borrowed(tool)
}

/// Broken-down local time of `t`: (year, month, day, hour, minute, second).
pub(crate) fn local_time(t: SystemTime) -> (i32, u32, u32, u32, u32, u32) {
    let secs = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) as libc::time_t;
    // SAFETY: tm is plain data and localtime_r fills it from the valid pointers.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&secs, &mut tm);
        tm
    };
    (
        tm.tm_year + 1900,
        (tm.tm_mon + 1) as u32,
        tm.tm_mday as u32,
        tm.tm_hour as u32,
        tm.tm_min as u32,
        tm.tm_sec as u32,
    )
}
