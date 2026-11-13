use std::borrow::Cow;
use std::ffi::OsStr;

/// The inverse of `as_encoded_bytes` for bytes the index stored from a name.
#[cfg(unix)]
pub(crate) fn os_str(bytes: &[u8]) -> Cow<'_, OsStr> {
    use std::os::unix::ffi::OsStrExt;
    Cow::Borrowed(OsStr::from_bytes(bytes))
}

/// Windows names are stored as the UTF-8 of `as_encoded_bytes`.
#[cfg(not(unix))]
pub(crate) fn os_str(bytes: &[u8]) -> Cow<'_, OsStr> {
    match String::from_utf8_lossy(bytes) {
        Cow::Borrowed(s) => Cow::Borrowed(OsStr::new(s)),
        Cow::Owned(s) => Cow::Owned(s.into()),
    }
}
