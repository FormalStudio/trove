use std::borrow::Cow;
use std::ffi::OsString;
use std::fs::{self, DirBuilder, File, FileType, Metadata, OpenOptions};
use std::io;
use std::os::windows::fs::{FileTypeExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::time::SystemTime;

use windows_sys::Win32::Foundation::ERROR_NOT_SAME_DEVICE;
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, GetFileInformationByHandle,
};

/// The raw error a rename across volumes fails with.
pub(crate) const CROSS_DEVICE: i32 = ERROR_NOT_SAME_DEVICE as i32;

pub(crate) const BAD_NAME: &str = "A name cannot contain \\ / : * ? \" < > |";

pub(crate) fn bad_name_char(c: char) -> bool {
    matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control()
}

/// The handle information of `path`; of the link itself when `m` says it is one.
fn info(m: &Metadata, path: &Path) -> Option<BY_HANDLE_FILE_INFORMATION> {
    let link = if m.file_type().is_symlink() { FILE_FLAG_OPEN_REPARSE_POINT } else { 0 };
    let f = OpenOptions::new().access_mode(0).custom_flags(FILE_FLAG_BACKUP_SEMANTICS | link).open(path).ok()?;
    // SAFETY: info is plain data and the handle is open for the call.
    unsafe {
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        (GetFileInformationByHandle(f.as_raw_handle(), &mut info) != 0).then_some(info)
    }
}

pub(crate) fn dev(m: &Metadata, path: &Path) -> Option<u64> {
    info(m, path).map(|i| u64::from(i.dwVolumeSerialNumber))
}

pub(crate) fn same_file(a: &Metadata, a_path: &Path, b: &Metadata, b_path: &Path) -> bool {
    let id = |i: BY_HANDLE_FILE_INFORMATION| (i.dwVolumeSerialNumber, i.nFileIndexHigh, i.nFileIndexLow);
    match (info(a, a_path), info(b, b_path)) {
        (Some(x), Some(y)) => id(x) == id(y),
        _ => false,
    }
}

/// Windows has a read-only flag where Unix has mode bits: read-only reads as 0o444 (0o555 for
/// folders), anything else as 0o644 (0o755).
pub(crate) fn mode(m: &Metadata) -> u32 {
    let exec = if m.is_dir() { 0o111 } else { 0 };
    (if m.permissions().readonly() { 0o444 } else { 0o644 }) | exec
}

/// Owners are SIDs, not uids.
pub(crate) fn owner(_: &Metadata) -> u32 {
    0
}

/// Only the owner's write bit maps to anything: the read-only flag.
pub(crate) fn set_file_mode(f: &File, mode: u32) -> io::Result<()> {
    let mut p = f.metadata()?.permissions();
    p.set_readonly(mode & 0o200 == 0);
    f.set_permissions(p)
}

pub(crate) fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    let mut p = fs::metadata(path)?.permissions();
    p.set_readonly(mode & 0o200 == 0);
    fs::set_permissions(path, p)
}

/// Permission bits for something being created; Windows has none to set.
pub(crate) trait ModeExt {
    fn unix_mode(&mut self, mode: u32) -> &mut Self;
}

impl ModeExt for OpenOptions {
    fn unix_mode(&mut self, _: u32) -> &mut Self {
        self
    }
}

impl ModeExt for DirBuilder {
    fn unix_mode(&mut self, _: u32) -> &mut Self {
        self
    }
}

/// Opens a reparse point itself rather than what it points at.
pub(crate) fn no_follow(o: &mut OpenOptions) -> &mut OpenOptions {
    o.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
}

/// A folder link when `target`, taken from beside `link`, is a folder now.
pub(crate) fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    let resolved = link.parent().map_or_else(|| target.to_path_buf(), |p| p.join(target));
    if resolved.is_dir() {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    }
}

/// Recreates the link `ft` describes, pointing at `target`.
pub(crate) fn copy_symlink(ft: &FileType, target: &Path, link: &Path) -> io::Result<()> {
    if ft.is_symlink_dir() {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    }
}

pub(crate) fn uid() -> u32 {
    0
}

/// The read-only flag is anyone's to change who may write the file.
pub(crate) fn may_change_mode(_: u32) -> bool {
    true
}

pub(crate) fn home() -> Option<OsString> {
    std::env::var_os("USERPROFILE")
}

/// `tool` as a file name on PATH.
pub(crate) fn exe_name(tool: &str) -> Cow<'_, str> {
    Cow::Owned(format!("{tool}.exe"))
}

/// Broken-down local time of `t`: (year, month, day, hour, minute, second).
pub(crate) fn local_time(t: SystemTime) -> (i32, u32, u32, u32, u32, u32) {
    let ts = jiff::Timestamp::try_from(t).unwrap_or(jiff::Timestamp::UNIX_EPOCH);
    let z = ts.to_zoned(jiff::tz::TimeZone::system());
    (i32::from(z.year()), z.month() as u32, z.day() as u32, z.hour() as u32, z.minute() as u32, z.second() as u32)
}
