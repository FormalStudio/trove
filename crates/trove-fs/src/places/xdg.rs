use std::fs;
use std::path::{Path, PathBuf};

use super::{Place, PlaceIcon, decode_path, parse_templates_dir, user_dir_value};

const USER_DIRS: &[(&str, &str, PlaceIcon)] = &[
    ("XDG_DESKTOP_DIR", "Desktop", PlaceIcon::Desktop),
    ("XDG_DOCUMENTS_DIR", "Documents", PlaceIcon::Documents),
    ("XDG_DOWNLOAD_DIR", "Downloads", PlaceIcon::Downloads),
    ("XDG_MUSIC_DIR", "Music", PlaceIcon::Music),
    ("XDG_PICTURES_DIR", "Pictures", PlaceIcon::Pictures),
    ("XDG_VIDEOS_DIR", "Videos", PlaceIcon::Videos),
];

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

pub fn trash_dir() -> PathBuf {
    data_home().join("Trash")
}

pub fn trash_files_dir() -> PathBuf {
    trash_dir().join("files")
}

pub fn recent(limit: usize) -> Vec<PathBuf> {
    let text = fs::read_to_string(data_home().join("recently-used.xbel")).unwrap_or_default();
    parse_recent(&text, limit)
}

pub(super) fn bookmarks_file() -> PathBuf {
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
        let _ = crate::places::bookmarks();
        let _ = recent(5);
        assert!(trash_dir().ends_with("Trash"));
    }
}
