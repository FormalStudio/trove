//! Names and metadata that differ by OS. Unix names are arbitrary bytes; Windows names are
//! UTF-16, carried here in the WTF-8 bytes of `as_encoded_bytes`, which is UTF-8 for any valid
//! name.

use std::borrow::Cow;
use std::ffi::OsString;
use std::fs::Metadata;
use std::path::{Path, PathBuf};

/// The inverse of `as_encoded_bytes` for bytes decoded from a URI or a file.
#[cfg(unix)]
pub fn os_string_from_bytes(bytes: Vec<u8>) -> OsString {
    use std::os::unix::ffi::OsStringExt;
    OsString::from_vec(bytes)
}

#[cfg(not(unix))]
pub fn os_string_from_bytes(bytes: Vec<u8>) -> OsString {
    String::from_utf8_lossy(&bytes).into_owned().into()
}

/// Permission bits, owner uid and group gid.
#[cfg(unix)]
pub fn mode_uid_gid(md: &Metadata) -> (u32, u32, u32) {
    use std::os::unix::fs::MetadataExt;
    (md.mode() & 0o7777, md.uid(), md.gid())
}

/// Windows has a read-only flag where Unix has mode bits: read-only files read as 0o444 (0o555
/// for folders), others as 0o644 (0o755). Owners are SIDs, reported as 0.
#[cfg(not(unix))]
pub fn mode_uid_gid(md: &Metadata) -> (u32, u32, u32) {
    let exec = if md.is_dir() { 0o111 } else { 0 };
    let mode = if md.permissions().readonly() { 0o444 } else { 0o644 };
    (mode | exec, 0, 0)
}

/// The path part of a `file://` URI, before percent-encoding.
#[cfg(unix)]
pub fn uri_bytes(path: &Path) -> Cow<'_, [u8]> {
    Cow::Borrowed(path.as_os_str().as_encoded_bytes())
}

/// `C:\Users\kyan` is `/C:/Users/kyan` in a URI.
#[cfg(not(unix))]
pub fn uri_bytes(path: &Path) -> Cow<'_, [u8]> {
    let text = path.to_string_lossy();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    let slashed = text.replace('\\', "/");
    let lead = if slashed.starts_with('/') { "" } else { "/" };
    Cow::Owned(format!("{lead}{slashed}").into_bytes())
}

/// The inverse of [`uri_bytes`] for the decoded path of a URI.
#[cfg(unix)]
pub fn path_from_uri_bytes(bytes: Vec<u8>) -> PathBuf {
    PathBuf::from(os_string_from_bytes(bytes))
}

#[cfg(not(unix))]
pub fn path_from_uri_bytes(bytes: Vec<u8>) -> PathBuf {
    let text = String::from_utf8_lossy(&bytes);
    let drive = text.strip_prefix('/').filter(|r| r.as_bytes().get(1) == Some(&b':'));
    PathBuf::from(drive.unwrap_or(&text).replace('/', "\\"))
}
