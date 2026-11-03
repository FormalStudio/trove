use std::fs;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
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

const USER_DIRS: &[(&str, &str, PlaceIcon)] = &[
    ("XDG_DESKTOP_DIR", "Desktop", PlaceIcon::Desktop),
    ("XDG_DOCUMENTS_DIR", "Documents", PlaceIcon::Documents),
    ("XDG_DOWNLOAD_DIR", "Downloads", PlaceIcon::Downloads),
    ("XDG_MUSIC_DIR", "Music", PlaceIcon::Music),
    ("XDG_PICTURES_DIR", "Pictures", PlaceIcon::Pictures),
    ("XDG_VIDEOS_DIR", "Videos", PlaceIcon::Videos),
];

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

fn home() -> PathBuf {
    std::env::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

fn env_dir(var: &str, fallback: impl FnOnce() -> PathBuf) -> PathBuf {
    match std::env::var_os(var) {
        Some(v) if Path::new(&v).is_absolute() => PathBuf::from(v),
        _ => fallback(),
    }
}

fn config_home() -> PathBuf {
    env_dir("XDG_CONFIG_HOME", || home().join(".config"))
}

fn data_home() -> PathBuf {
    env_dir("XDG_DATA_HOME", || home().join(".local/share"))
}

pub fn user_dirs() -> Vec<Place> {
    let text = fs::read_to_string(config_home().join("user-dirs.dirs")).unwrap_or_default();
    parse_user_dirs(&text, &home())
}

/// The templates folder, `None` when the user disabled it.
pub fn templates_dir() -> Option<PathBuf> {
    let text = fs::read_to_string(config_home().join("user-dirs.dirs")).unwrap_or_default();
    parse_templates_dir(&text, &home())
}

/// `XDG_TEMPLATES_DIR` from a `user-dirs.dirs` text, `~/Templates` when unset. xdg-user-dirs
/// points a disabled directory at `$HOME`, which reads as no folder.
pub fn parse_templates_dir(text: &str, home: &Path) -> Option<PathBuf> {
    let path = user_dir_value(text, "XDG_TEMPLATES_DIR", home).unwrap_or_else(|| home.join("Templates"));
    (path != home).then_some(path)
}

pub fn bookmarks() -> Vec<Place> {
    read_bookmarks(&bookmarks_file())
}

pub fn add_bookmark(path: &Path, label: Option<&str>) -> io::Result<()> {
    add_bookmark_in(&bookmarks_file(), path, label)
}

pub fn remove_bookmark(path: &Path) -> io::Result<()> {
    remove_bookmark_in(&bookmarks_file(), path)
}

pub fn trash_dir() -> PathBuf {
    data_home().join("Trash")
}

pub fn recent(limit: usize) -> Vec<PathBuf> {
    let text = fs::read_to_string(data_home().join("recently-used.xbel")).unwrap_or_default();
    parse_recent(&text, limit)
}

fn bookmarks_file() -> PathBuf {
    config_home().join("gtk-3.0/bookmarks")
}

pub(crate) fn parse_user_dirs(text: &str, home: &Path) -> Vec<Place> {
    let mut places = vec![Place { label: "Home".into(), path: home.to_path_buf(), icon: PlaceIcon::Home }];
    for (key, default, icon) in USER_DIRS {
        let path = user_dir_value(text, key, home).unwrap_or_else(|| home.join(default));
        // xdg-user-dirs points a disabled directory at $HOME.
        if path == home {
            continue;
        }
        let label = path.file_name().map_or_else(|| (*default).to_string(), |n| n.to_string_lossy().into_owned());
        places.push(Place { label, path, icon: *icon });
    }
    places
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
fn decode_path(after_scheme: &str) -> Option<PathBuf> {
    let path = if after_scheme.starts_with('/') {
        after_scheme
    } else {
        after_scheme.strip_prefix("localhost").filter(|p| p.starts_with('/'))?
    };
    let bytes: Vec<u8> = percent_decode_str(path).collect();
    Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

/// `path` as a `file://` URI, every byte outside the path set percent-encoded.
pub fn file_uri(path: &Path) -> String {
    format!("file://{}", percent_encode(path.as_os_str().as_bytes(), URI_PATH))
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

/// `<bookmark href=... modified=...>` elements of recently-used.xbel, newest first.
pub(crate) fn parse_recent(xbel: &str, limit: usize) -> Vec<PathBuf> {
    let mut found: Vec<(&str, PathBuf)> = Vec::new();
    for chunk in xbel.split("<bookmark ").skip(1) {
        let Some(tag) = chunk.split('>').next() else { continue };
        let Some(href) = attr(tag, "href") else { continue };
        let Some(rest) = xml_unescape(href).strip_prefix("file://").map(str::to_string) else { continue };
        let Some(path) = decode_path(&rest) else { continue };
        let when = attr(tag, "modified").or_else(|| attr(tag, "added")).unwrap_or("");
        found.push((when, path));
    }
    found.sort_by(|a, b| b.0.cmp(a.0));
    let mut seen = std::collections::HashSet::new();
    found.into_iter().filter(|(_, p)| seen.insert(p.clone())).map(|(_, p)| p).take(limit).collect()
}

fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let needle = format!("{name}=\"");
    let start = tag.match_indices(&needle).find(|(i, _)| *i == 0 || tag.as_bytes()[i - 1] == b' ')?.0 + needle.len();
    let len = tag[start..].find('"')?;
    Some(&tag[start..start + len])
}

fn xml_unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.find(';') else { break };
        let ent = &rest[1..end];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => ent
                .strip_prefix("#x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIRS: &str = "\
# This file is written by xdg-user-dirs-update
XDG_DESKTOP_DIR=\"$HOME/Desktop\"
XDG_DOWNLOAD_DIR=\"$HOME/Downloads\"
XDG_TEMPLATES_DIR=\"$HOME/Templates\"
XDG_DOCUMENTS_DIR=\"$HOME/Documents\"
XDG_MUSIC_DIR=\"/data/My \\\"Music\\\"\"
XDG_PICTURES_DIR=\"$HOME/\"
XDG_VIDEOS_DIR=\"${HOME}/Clips\"
";

    fn labels(p: &[Place]) -> Vec<&str> {
        p.iter().map(|p| p.label.as_str()).collect()
    }

    #[test]
    fn user_dirs_parse() {
        let home = Path::new("/home/kyan");
        let p = parse_user_dirs(DIRS, home);
        assert_eq!(labels(&p), ["Home", "Desktop", "Documents", "Downloads", "My \"Music\"", "Clips"]);
        assert_eq!(p[0].path, home);
        assert_eq!(p[1].path, Path::new("/home/kyan/Desktop"));
        assert_eq!(p[4].path, Path::new("/data/My \"Music\""));
        assert_eq!(p[5].icon, PlaceIcon::Videos);
        assert!(!p.iter().any(|p| p.icon == PlaceIcon::Pictures), "a dir equal to HOME is disabled");
    }

    #[test]
    fn user_dirs_default_when_missing() {
        let p = parse_user_dirs("", Path::new("/home/kyan"));
        assert_eq!(labels(&p), ["Home", "Desktop", "Documents", "Downloads", "Music", "Pictures", "Videos"]);
        assert_eq!(p[6].path, Path::new("/home/kyan/Videos"));
    }

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

    #[test]
    fn recent_newest_first() {
        let xbel = r#"<?xml version="1.0" encoding="UTF-8"?>
<xbel version="1.0">
  <bookmark href="file:///home/kyan/old.png" added="2026-06-18T10:02:56Z" modified="2026-06-18T10:02:56.1Z" visited="x">
    <info><metadata owner="http://freedesktop.org">
      <bookmark:applications><bookmark:application name="loupe" exec="&apos;loupe %u&apos;" modified="2026-09-01T00:00:00Z" count="2"/></bookmark:applications>
    </metadata></info>
  </bookmark>
  <bookmark href="file:///home/kyan/R&amp;D/new%20file.txt" added="2026-06-19T11:39:05Z" modified="2026-07-06T14:16:38.35Z">
  </bookmark>
  <bookmark href="https://example.com/x" added="2026-08-01T00:00:00Z" modified="2026-08-01T00:00:00Z"/>
  <bookmark href="file:///home/kyan/mid.txt" added="2026-06-19T11:39:05Z" modified="2026-06-30T00:00:00Z"/>
</xbel>"#;
        let r = parse_recent(xbel, 10);
        assert_eq!(
            r,
            [
                Path::new("/home/kyan/R&D/new file.txt"),
                Path::new("/home/kyan/mid.txt"),
                Path::new("/home/kyan/old.png")
            ]
        );
        assert_eq!(parse_recent(xbel, 1).len(), 1);
        assert!(parse_recent("", 5).is_empty());
    }

    #[test]
    fn xml_entities() {
        assert_eq!(xml_unescape("a&amp;b &lt;c&gt; &#65;&#x42; &bogus; &"), "a&b <c> AB &bogus; &");
    }

    #[test]
    fn live_places_do_not_panic() {
        assert_eq!(user_dirs().first().map(|p| p.icon), Some(PlaceIcon::Home));
        let _ = bookmarks();
        let _ = recent(5);
        assert!(trash_dir().ends_with("Trash"));
    }
}
