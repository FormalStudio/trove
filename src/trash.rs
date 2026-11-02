//! The Trash view's extra columns: where each trashed item came from and when it was deleted,
//! read from the `.trashinfo` file beside `files/` on a worker thread.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// One item's `.trashinfo`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrashInfo {
    /// The folder the item was in.
    pub origin: Option<PathBuf>,
    /// `DeletionDate` as written, `2026-10-30T09:41:00`.
    pub deleted: Option<String>,
}

/// Whether `dir` is a trash's `files` folder: the home one, or one on a volume.
pub fn is_trash_files(dir: &Path, home_trash: &Path) -> bool {
    if dir == home_trash {
        return true;
    }
    dir.file_name().is_some_and(|n| n == "files")
        && dir.parent().and_then(Path::file_name).is_some_and(|n| {
            let n = n.to_string_lossy();
            n.starts_with(".Trash-") || n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty()
        })
}

/// Reads the info of each name in `files`, a trash's `files` folder.
pub fn read_infos(files: &Path, names: &[OsString]) -> Vec<(OsString, TrashInfo)> {
    let Some(trash) = files.parent() else { return Vec::new() };
    let info_dir = trash.join("info");
    // A volume trash records paths relative to the volume's top folder.
    let top = top_dir(trash);
    names
        .iter()
        .map(|name| {
            let mut info = name.clone();
            info.push(".trashinfo");
            let text = std::fs::read_to_string(info_dir.join(info)).unwrap_or_default();
            (name.clone(), parse(&text, top.as_deref()))
        })
        .collect()
}

/// `$topdir` of a volume trash (`$topdir/.Trash-1000` or `$topdir/.Trash/1000`).
fn top_dir(trash: &Path) -> Option<PathBuf> {
    let name = trash.file_name()?.to_string_lossy();
    if name.starts_with(".Trash-") {
        return trash.parent().map(Path::to_path_buf);
    }
    let parent = trash.parent()?;
    (parent.file_name()? == ".Trash").then(|| parent.parent().map(Path::to_path_buf)).flatten()
}

fn parse(text: &str, top: Option<&Path>) -> TrashInfo {
    let mut info = TrashInfo::default();
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("Path=") {
            let path = PathBuf::from(decode(v.trim()));
            let full = match top {
                Some(top) if path.is_relative() => top.join(path),
                _ => path,
            };
            info.origin = full.parent().map(Path::to_path_buf);
        } else if let Some(v) = line.strip_prefix("DeletionDate=") {
            info.deleted = Some(v.trim().to_owned());
        }
    }
    info
}

fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Some(b) = std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// "30 Oct 2026, 09:41" style text for a `DeletionDate`, through the same words the Modified
/// column uses.
pub fn deleted_label(deleted: &str, today: &crate::format::Today) -> String {
    let civil = deleted.parse::<jiff::civil::DateTime>().ok();
    let when = civil
        .and_then(|c| c.to_zoned(jiff::tz::TimeZone::system()).ok())
        .map(|z| std::time::SystemTime::from(z.timestamp()));
    when.map(|t| today.modified(t)).unwrap_or_else(|| deleted.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_home_and_volume_infos() {
        let info = parse("[Trash Info]\nPath=/home/u/Docs/a%20b.txt\nDeletionDate=2026-10-30T09:41:00\n", None);
        assert_eq!(info.origin, Some(PathBuf::from("/home/u/Docs")));
        assert_eq!(info.deleted.as_deref(), Some("2026-10-30T09:41:00"));
        let rel = parse("Path=photos/x.jpg\n", Some(Path::new("/run/media/u/USB")));
        assert_eq!(rel.origin, Some(PathBuf::from("/run/media/u/USB/photos")));
    }

    #[test]
    fn knows_trash_folders() {
        let home = Path::new("/h/.local/share/Trash/files");
        assert!(is_trash_files(home, home));
        assert!(is_trash_files(Path::new("/run/media/u/USB/.Trash-1000/files"), home));
        assert!(is_trash_files(Path::new("/mnt/.Trash/1000/files"), home));
        assert!(!is_trash_files(Path::new("/h/Documents/files"), home));
    }
}
