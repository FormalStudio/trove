use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use percent_encoding::{AsciiSet, CONTROLS, percent_decode_str, percent_encode};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PlaceIcon {
    Home,
    Desktop,
    Documents,
    Downloads,
    Music,
    Pictures,
    Videos,
    Folder,
    Network,
    Trash,
    Recent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    pub label: String,
    pub path: PathBuf,
    pub icon: PlaceIcon,
}

/// What GLib leaves unescaped in a file URI path.
const URI_PATH: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

/// `XDG_TEMPLATES_DIR` from a `user-dirs.dirs` text, `~/Templates` when unset. xdg-user-dirs
/// points a disabled directory at `$HOME`, which reads as no folder.
pub fn parse_templates_dir(text: &str, home: &Path) -> Option<PathBuf> {
    let path = user_dir_value(text, "XDG_TEMPLATES_DIR", home).unwrap_or_else(|| home.join("Templates"));
    (path != home).then_some(path)
}

#[cfg(not(target_os = "linux"))]
mod native;
#[cfg(target_os = "linux")]
mod xdg;

#[cfg(not(target_os = "linux"))]
use native as sys;
#[cfg(target_os = "linux")]
use xdg as sys;

pub use sys::{recent, templates_dir, trash_dir, trash_files_dir, user_dirs};

pub fn bookmarks() -> Vec<Place> {
    read_bookmarks(&sys::bookmarks_file())
}

pub fn add_bookmark(path: &Path, label: Option<&str>) -> io::Result<()> {
    add_bookmark_in(&sys::bookmarks_file(), path, label)
}

pub fn remove_bookmark(path: &Path) -> io::Result<()> {
    remove_bookmark_in(&sys::bookmarks_file(), path)
}

fn user_dir_value(text: &str, key: &str, home: &Path) -> Option<PathBuf> {
    let raw = text.lines().rev().find_map(|line| {
        let (k, v) = line.trim().split_once('=')?;
        (k.trim() == key).then_some(v.trim())
    })?;
    let value = shell_unquote(raw);
    let home = home.to_string_lossy();
    let expanded = if let Some(rest) = value.strip_prefix("${HOME}") {
        format!("{home}{rest}")
    } else if let Some(rest) = value.strip_prefix("$HOME") {
        format!("{home}{rest}")
    } else {
        value
    };
    let path = PathBuf::from(&expanded);
    if expanded.is_empty() {
        None
    } else if path.is_absolute() {
        Some(path)
    } else {
        Some(Path::new(&*home).join(path))
    }
}

fn shell_unquote(s: &str) -> String {
    let inner =
        if s.len() >= 2 && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\''))) {
            &s[1..s.len() - 1]
        } else {
            s
        };
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(n) = chars.next() {
                out.push(n);
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub(crate) fn parse_bookmarks(text: &str) -> Vec<Place> {
    text.lines().filter_map(parse_bookmark_line).collect()
}

fn parse_bookmark_line(line: &str) -> Option<Place> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let (uri, label) = match line.split_once(' ') {
        Some((u, l)) => (u, Some(l.trim()).filter(|l| !l.is_empty())),
        None => (line, None),
    };
    match uri.strip_prefix("file://") {
        Some(rest) => {
            let path = decode_path(rest)?;
            let label = label.map(str::to_string).unwrap_or_else(|| name_of(&path));
            Some(Place { label, path, icon: PlaceIcon::Folder })
        }
        None => {
            let label = label.map(str::to_string).unwrap_or_else(|| remote_label(uri));
            Some(Place { label, path: PathBuf::from(uri), icon: PlaceIcon::Network })
        }
    }
}

fn name_of(path: &Path) -> String {
    path.file_name().map_or_else(|| path.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned())
}

fn remote_label(uri: &str) -> String {
    let (_, rest) = uri.split_once("://").unwrap_or(("", uri));
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let last = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    let shown = if last.is_empty() { host } else { last };
    percent_decode_str(shown).decode_utf8_lossy().into_owned()
}

/// The path a `file://` URI names; `None` for any other scheme or a remote host.
pub fn uri_path(uri: &str) -> Option<PathBuf> {
    decode_path(uri.strip_prefix("file://")?)
}

/// The part of a `file://` URI after the scheme: an empty or `localhost` host, then the path.
pub(crate) fn decode_path(after_scheme: &str) -> Option<PathBuf> {
    let path = if after_scheme.starts_with('/') {
        after_scheme
    } else {
        after_scheme.strip_prefix("localhost").filter(|p| p.starts_with('/'))?
    };
    Some(crate::os::path_from_uri_bytes(percent_decode_str(path).collect()))
}

/// `path` as a `file://` URI, every byte outside the path set percent-encoded.
pub fn file_uri(path: &Path) -> String {
    format!("file://{}", percent_encode(&crate::os::uri_bytes(path), URI_PATH))
}

fn read_bookmarks(file: &Path) -> Vec<Place> {
    parse_bookmarks(&fs::read_to_string(file).unwrap_or_default())
}

pub(crate) fn add_bookmark_in(file: &Path, path: &Path, label: Option<&str>) -> io::Result<()> {
    let mut text = match fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    if parse_bookmarks(&text).iter().any(|p| p.icon != PlaceIcon::Network && p.path == path) {
        return Ok(());
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&file_uri(path));
    if let Some(l) = label.map(str::trim).filter(|l| !l.is_empty()) {
        text.push(' ');
        text.push_str(l);
    }
    text.push('\n');
    write_atomic(file, &text)
}

pub(crate) fn remove_bookmark_in(file: &Path, path: &Path) -> io::Result<()> {
    let text = match fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let keep = |line: &str| match parse_bookmark_line(line) {
        Some(p) => !(p.icon != PlaceIcon::Network && p.path == path),
        None => true,
    };
    let kept: Vec<&str> = text.lines().filter(|l| keep(l)).collect();
    if kept.len() == text.lines().count() {
        return Ok(());
    }
    let mut out = kept.join("\n");
    if !kept.is_empty() {
        out.push('\n');
    }
    write_atomic(file, &out)
}

fn write_atomic(file: &Path, text: &str) -> io::Result<()> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut tmp = file.as_os_str().to_owned();
    tmp.push(format!(".tmp{}", std::process::id()));
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, text)?;
    fs::rename(&tmp, file).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bookmarks_parse() {
        let text = "\
file:///home/kyan/macbook Macbook
file:///home/kyan/My%20Docs
# comment

sftp://nas.local/volume1/share%20x Nas Share
smb://server/pub
file://localhost/srv/data  Data  disk
file://otherhost/srv/x
";
        let p = parse_bookmarks(text);
        assert_eq!(p.len(), 5);
        assert_eq!(
            (p[0].label.as_str(), p[0].path.as_path(), p[0].icon),
            ("Macbook", Path::new("/home/kyan/macbook"), PlaceIcon::Folder)
        );
        assert_eq!((p[1].label.as_str(), p[1].path.as_path()), ("My Docs", Path::new("/home/kyan/My Docs")));
        assert_eq!((p[2].label.as_str(), p[2].icon), ("Nas Share", PlaceIcon::Network));
        assert_eq!(p[2].path, Path::new("sftp://nas.local/volume1/share%20x"));
        assert_eq!(p[3].label, "pub");
        assert_eq!(p[4].label, "Data  disk");
        assert_eq!(p[4].path, Path::new("/srv/data"));
    }

    #[test]
    fn bookmark_add_remove_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("gtk-3.0/bookmarks");
        add_bookmark_in(&file, Path::new("/home/kyan/Projects"), None).expect("add");
        add_bookmark_in(&file, Path::new("/home/kyan/My Stuff #1"), Some("Stuff")).expect("add");
        add_bookmark_in(&file, Path::new("/home/kyan/Projects"), Some("again")).expect("dup add");
        let text = fs::read_to_string(&file).expect("read");
        assert_eq!(text, "file:///home/kyan/Projects\nfile:///home/kyan/My%20Stuff%20%231 Stuff\n");
        let p = read_bookmarks(&file);
        assert_eq!(p[1].path, Path::new("/home/kyan/My Stuff #1"));

        fs::write(&file, "# keep me\nsftp://h/x X\nfile:///a/b\n\nfile:///c%20d D").expect("seed");
        remove_bookmark_in(&file, Path::new("/c d")).expect("remove");
        assert_eq!(fs::read_to_string(&file).expect("read"), "# keep me\nsftp://h/x X\nfile:///a/b\n\n");
        remove_bookmark_in(&file, Path::new("/nope")).expect("noop");
        remove_bookmark_in(&dir.path().join("absent"), Path::new("/a")).expect("missing file");
        assert!(fs::read_dir(file.parent().expect("parent")).expect("dir").count() == 1, "no tmp files left");
    }
}
