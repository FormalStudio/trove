use std::path::Path;

/// Bytes left as they are in a path: RFC 3986 unreserved plus the sub-delims, `:`, `@` and `/`,
/// which is the set GLib leaves alone. Matching it keeps the md5 keys identical to GNOME's.
fn is_path_safe(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'-' | b'.'
                | b'_'
                | b'~'
                | b'!'
                | b'$'
                | b'&'
                | b'\''
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b';'
                | b'='
                | b':'
                | b'@'
                | b'/'
        )
}

/// `file://` URI of an absolute path. Symlinks are not resolved: the spec keys on the path an
/// app was given.
pub(crate) fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut uri = String::from("file://");
    for &b in abs.as_os_str().as_bytes() {
        if is_path_safe(b) {
            uri.push(char::from(b));
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    uri
}

pub(crate) fn key_of(uri: &str) -> String {
    format!("{:x}", md5::compute(uri.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_example_key() {
        let uri = file_uri(Path::new("/home/jens/photos/me.png"));
        assert_eq!(uri, "file:///home/jens/photos/me.png");
        assert_eq!(key_of(&uri), "c6ee772d9e49320e97ec29a7eb5b1697");
    }

    #[test]
    fn reserved_bytes_are_percent_encoded() {
        assert_eq!(file_uri(Path::new("/a b/ü#?%.png")), "file:///a%20b/%C3%BC%23%3F%25.png");
        assert_eq!(file_uri(Path::new("/it's (1)+2.png")), "file:///it's%20(1)+2.png");
    }
}
