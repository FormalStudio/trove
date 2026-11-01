use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

/// Stable key for one name inside one directory: the list's row key. A hash of the name, so it
/// survives re-listing and re-sorting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EntryId(pub u64);

impl EntryId {
    pub fn of(name: &str) -> Self {
        // FNV-1a: fast, stable across runs (std's SipHash keys are random per process).
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in name.as_bytes() {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        Self(h)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EntryKind {
    Dir,
    File,
    /// A symlink; `to_dir` once its target has been stat'd and is a directory.
    Symlink {
        to_dir: bool,
        broken: bool,
    },
    Other,
}

impl EntryKind {
    pub fn is_dir(self) -> bool {
        matches!(self, Self::Dir | Self::Symlink { to_dir: true, .. })
    }
}

/// Metadata that needs a stat. Absent until the listing's second phase reaches the entry, which
/// on a slow network mount can be well after the name is on screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Meta {
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub created: Option<SystemTime>,
    pub accessed: Option<SystemTime>,
    /// st_mode permission bits.
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub kind: EntryKind,
    /// Number of entries, for a directory whose listing has been counted. Never computed by stat.
    pub child_count: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub id: EntryId,
    pub name: Arc<str>,
    /// From the directory entry's d_type, so known before any stat.
    pub kind: EntryKind,
    pub file_kind: FileKind,
    pub hidden: bool,
    pub meta: Option<Meta>,
}

impl Entry {
    pub fn new(name: &str, kind: EntryKind) -> Self {
        Self {
            id: EntryId::of(name),
            name: Arc::from(name),
            kind,
            file_kind: if kind.is_dir() { FileKind::Folder } else { FileKind::from_name(name) },
            hidden: name.starts_with('.') || name.ends_with('~'),
            meta: None,
        }
    }

    pub fn is_dir(&self) -> bool {
        self.meta.as_ref().map_or(self.kind, |m| m.kind).is_dir()
    }

    pub fn path_in(&self, dir: &Path) -> PathBuf {
        dir.join(&*self.name)
    }

    /// Lowercased extension without the dot, empty for none or for a dotfile like `.bashrc`.
    pub fn extension(&self) -> &str {
        extension_of(&self.name)
    }
}

pub fn extension_of(name: &str) -> &str {
    match name.rfind('.') {
        Some(0) | None => "",
        Some(i) => &name[i + 1..],
    }
}

/// What a file is, from its name alone: no content sniffing, so it costs nothing per row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FileKind {
    Folder,
    Image,
    Video,
    Audio,
    Text,
    Code,
    Pdf,
    Document,
    Spreadsheet,
    Presentation,
    Archive,
    DiskImage,
    Font,
    Executable,
    Unknown,
}

impl FileKind {
    pub fn from_name(name: &str) -> Self {
        let ext = extension_of(name).to_ascii_lowercase();
        match ext.as_str() {
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "svg" | "ico" | "avif"
            | "heic" | "heif" | "jxl" | "qoi" | "tga" | "psd" | "xcf" | "raw" | "cr2" | "nef" | "arw"
            | "dng" => Self::Image,
            "mp4" | "mkv" | "webm" | "mov" | "avi" | "m4v" | "wmv" | "flv" | "mpg" | "mpeg" | "ogv"
            | "3gp" => Self::Video,
            "mp3" | "flac" | "ogg" | "opus" | "wav" | "m4a" | "aac" | "wma" | "aiff" | "alac" | "mka" => {
                Self::Audio
            }
            "txt" | "md" | "markdown" | "rst" | "log" | "csv" | "tsv" | "org" | "nfo" | "srt" | "vtt" => {
                Self::Text
            }
            "rs" | "c" | "h" | "cc" | "cpp" | "hpp" | "cxx" | "py" | "js" | "mjs" | "cjs" | "jsx"
            | "tsx" | "go" | "java" | "kt" | "swift" | "rb" | "php" | "lua" | "sh" | "bash" | "zsh"
            | "fish" | "nix" | "star" | "toml" | "yaml" | "yml" | "json" | "jsonc" | "xml" | "html"
            | "htm" | "css" | "scss" | "sql" | "zig" | "hs" | "ml" | "ex" | "exs" | "erl" | "cs"
            | "dart" | "vue" | "svelte" | "astro" | "ini" | "conf" | "cfg" | "desktop" | "service"
            | "diff" | "patch" | "cmake" | "mk" | "gradle" | "tf" | "proto" | "graphql" | "el"
            | "vim" | "ps1" | "bat" | "r" | "jl" | "scala" | "clj" | "lock"
            // TypeScript, not MPEG transport stream: by far the likelier `.ts` in a home directory.
            | "ts" => Self::Code,
            "pdf" => Self::Pdf,
            "doc" | "docx" | "odt" | "rtf" | "pages" | "epub" | "tex" => Self::Document,
            "xls" | "xlsx" | "ods" | "numbers" => Self::Spreadsheet,
            "ppt" | "pptx" | "odp" | "key" => Self::Presentation,
            "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "7z" | "rar" | "lz" | "lzma" | "cab"
            | "deb" | "rpm" | "apk" | "jar" | "whl" | "crate" => Self::Archive,
            "iso" | "img" | "dmg" | "qcow2" | "vdi" | "vmdk" | "raw.xz" => Self::DiskImage,
            "ttf" | "otf" | "woff" | "woff2" | "pcf" | "bdf" => Self::Font,
            "appimage" | "exe" | "msi" | "bin" | "run" | "flatpakref" => Self::Executable,
            _ => match name {
                "Makefile" | "Dockerfile" | "Justfile" | "justfile" | "Containerfile" | "PKGBUILD"
                | "CMakeLists.txt" | "Cargo.lock" | "flake.lock" => Self::Code,
                "README" | "LICENSE" | "COPYING" | "AUTHORS" | "CHANGELOG" | "TODO" => Self::Text,
                _ => Self::Unknown,
            },
        }
    }

    /// The Kind column's words.
    pub fn label(self) -> &'static str {
        match self {
            Self::Folder => "Folder",
            Self::Image => "Image",
            Self::Video => "Video",
            Self::Audio => "Audio",
            Self::Text => "Text",
            Self::Code => "Source code",
            Self::Pdf => "PDF document",
            Self::Document => "Document",
            Self::Spreadsheet => "Spreadsheet",
            Self::Presentation => "Presentation",
            Self::Archive => "Archive",
            Self::DiskImage => "Disk image",
            Self::Font => "Font",
            Self::Executable => "Application",
            Self::Unknown => "Document",
        }
    }
}

/// Finder's ordering: case-insensitive, and runs of digits compared as numbers, so `file2` sorts
/// before `file10`.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.as_bytes(), b.as_bytes());
    loop {
        match (a.first(), b.first()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let da = a.iter().take_while(|c| c.is_ascii_digit()).count();
                let db = b.iter().take_while(|c| c.is_ascii_digit()).count();
                let (na, nb) = (trim_zeros(&a[..da]), trim_zeros(&b[..db]));
                let ord = na.len().cmp(&nb.len()).then_with(|| na.cmp(nb)).then_with(|| da.cmp(&db));
                if ord != Ordering::Equal {
                    return ord;
                }
                a = &a[da..];
                b = &b[db..];
            }
            (Some(_), Some(_)) => {
                let ca = next_char(a);
                let cb = next_char(b);
                let ord = fold(ca.0).cmp(&fold(cb.0));
                if ord != Ordering::Equal {
                    return ord;
                }
                a = &a[ca.1..];
                b = &b[cb.1..];
            }
        }
    }
}

fn trim_zeros(d: &[u8]) -> &[u8] {
    let n = d.iter().take_while(|c| **c == b'0').count();
    &d[n.min(d.len().saturating_sub(1))..]
}

fn next_char(s: &[u8]) -> (char, usize) {
    let len = match s[0] {
        b if b < 0x80 => 1,
        b if b >> 5 == 0b110 => 2,
        b if b >> 4 == 0b1110 => 3,
        _ => 4,
    }
    .min(s.len());
    let c = std::str::from_utf8(&s[..len]).ok().and_then(|t| t.chars().next()).unwrap_or('\u{fffd}');
    (c, len)
}

fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum SortKey {
    #[default]
    Name,
    Size,
    Kind,
    Modified,
    Created,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SortOrder {
    pub key: SortKey,
    pub ascending: bool,
    pub folders_first: bool,
}

impl Default for SortOrder {
    fn default() -> Self {
        Self { key: SortKey::Name, ascending: true, folders_first: true }
    }
}

/// Sorts in place. Entries whose metadata has not arrived sort as size 0 and no date, then by
/// name, so the order is total and stable while stats stream in.
pub fn sort_entries(entries: &mut [Entry], order: SortOrder) {
    entries.sort_by(|a, b| {
        if order.folders_first {
            let d = b.is_dir().cmp(&a.is_dir());
            if d != Ordering::Equal {
                return d;
            }
        }
        let by_key = match order.key {
            SortKey::Name => Ordering::Equal,
            SortKey::Size => size_of(a).cmp(&size_of(b)),
            SortKey::Kind => {
                a.file_kind.label().cmp(b.file_kind.label()).then_with(|| a.extension().cmp(b.extension()))
            }
            SortKey::Modified => meta_time(a, |m| m.modified).cmp(&meta_time(b, |m| m.modified)),
            SortKey::Created => meta_time(a, |m| m.created).cmp(&meta_time(b, |m| m.created)),
        };
        let ord = by_key.then_with(|| natural_cmp(&a.name, &b.name)).then_with(|| a.name.cmp(&b.name));
        if order.ascending { ord } else { ord.reverse() }
    });
}

fn size_of(e: &Entry) -> u64 {
    e.meta.as_ref().map_or(0, |m| if m.kind.is_dir() { u64::from(m.child_count.unwrap_or(0)) } else { m.size })
}

fn meta_time(e: &Entry, f: impl Fn(&Meta) -> Option<SystemTime>) -> Option<SystemTime> {
    e.meta.as_ref().and_then(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut v = vec!["file10", "File2", "file1", "file02", "apple", "Banana"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["apple", "Banana", "file1", "File2", "file02", "file10"]);
    }

    #[test]
    fn kinds() {
        assert_eq!(FileKind::from_name("photo.JPG"), FileKind::Image);
        assert_eq!(FileKind::from_name(".bashrc"), FileKind::Unknown);
        assert_eq!(FileKind::from_name("main.ts"), FileKind::Code);
        assert_eq!(FileKind::from_name("Makefile"), FileKind::Code);
        assert_eq!(extension_of("archive.tar.gz"), "gz");
    }
}
