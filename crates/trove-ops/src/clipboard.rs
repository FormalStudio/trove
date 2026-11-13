//! File clipboard. Always kept in process, and offered to the system as
//! `x-special/gnome-copied-files`: on Linux through `wl-copy` (see `wayland.rs`), on macOS and
//! Windows by backends that turn that payload into the system's own file list and back.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};

const GNOME: &str = "x-special/gnome-copied-files";
const URI_LIST: &str = "text/uri-list";
const KDE_CUT: &str = "x-kde-cut-selection";
const PATH_SET: &AsciiSet = &NON_ALPHANUMERIC.remove(b'/').remove(b'-').remove(b'_').remove(b'.').remove(b'~');

/// The system clipboard. Faked in tests.
pub trait Backend: Send + Sync {
    /// Offers `data` as `mime`; false when the system clipboard cannot be set.
    fn offer(&self, mime: &str, data: &[u8]) -> bool;
    /// The types on offer; `None` when the system clipboard cannot be read at all.
    fn types(&self) -> Option<Vec<String>>;
    fn read(&self, mime: &str) -> Option<Vec<u8>>;
}

pub struct Clipboard {
    backend: Box<dyn Backend>,
    local: Mutex<Option<Local>>,
}

struct Local {
    paths: Vec<PathBuf>,
    cut: bool,
    /// The system clipboard was set too, so anything else on it now is newer than this.
    mirrored: bool,
}

impl Clipboard {
    pub fn new(backend: Box<dyn Backend>) -> Self {
        Self { backend, local: Mutex::new(None) }
    }

    pub fn copy_files(&self, paths: &[PathBuf], cut: bool) {
        let mirrored = !paths.is_empty() && self.backend.offer(GNOME, gnome_payload(paths, cut).as_bytes());
        let mut local = self.local.lock().unwrap_or_else(|e| e.into_inner());
        *local = (!paths.is_empty()).then(|| Local { paths: paths.to_vec(), cut, mirrored });
    }

    pub fn paste_files(&self) -> Option<(Vec<PathBuf>, bool)> {
        let types = self.backend.types();
        if let Some(types) = &types
            && let Some(found) = self.read_system(types)
        {
            return Some(found);
        }
        let local = self.local.lock().unwrap_or_else(|e| e.into_inner());
        let local = local.as_ref()?;
        if local.mirrored && types.is_some() {
            return None;
        }
        Some((local.paths.clone(), local.cut))
    }

    pub fn clear(&self) {
        *self.local.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    fn read_system(&self, types: &[String]) -> Option<(Vec<PathBuf>, bool)> {
        let has = |t: &str| types.iter().any(|x| x == t);
        if has(GNOME)
            && let Some(data) = self.backend.read(GNOME)
            && let Some(found) = parse_gnome(&String::from_utf8_lossy(&data))
        {
            return Some(found);
        }
        if has(URI_LIST)
            && let Some(data) = self.backend.read(URI_LIST)
        {
            let paths = parse_uri_list(&String::from_utf8_lossy(&data));
            if !paths.is_empty() {
                let cut = has(KDE_CUT) && self.backend.read(KDE_CUT).is_some_and(|d| d.starts_with(b"1"));
                return Some((paths, cut));
            }
        }
        let text_type = types.iter().find(|t| t.starts_with("text/plain"))?;
        let data = self.backend.read(text_type)?;
        let text = String::from_utf8_lossy(&data);
        let paths: Vec<PathBuf> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| PathBuf::from(l.strip_prefix("file://").map_or_else(|| l.to_owned(), decode)))
            .collect();
        (!paths.is_empty() && paths.iter().all(|p| p.is_absolute() && p.exists())).then_some((paths, false))
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
#[path = "clipboard/wayland.rs"]
mod system;
#[cfg(target_os = "macos")]
#[path = "clipboard/macos.rs"]
mod system;
#[cfg(windows)]
#[path = "clipboard/windows.rs"]
mod system;

fn decode(s: &str) -> String {
    percent_decode_str(s).decode_utf8_lossy().into_owned()
}

fn encode_path(p: &Path) -> String {
    utf8_percent_encode(&String::from_utf8_lossy(&trove_fs::os::uri_bytes(p)), PATH_SET).to_string()
}

fn gnome_payload(paths: &[PathBuf], cut: bool) -> String {
    let mut out = String::from(if cut { "cut" } else { "copy" });
    for p in paths {
        out.push_str("\nfile://");
        out.push_str(&encode_path(p));
    }
    out
}

fn file_uri_path(line: &str) -> Option<PathBuf> {
    let rest = line.trim().strip_prefix("file://")?;
    // "file://host/path" and "file:///path": the path starts at the first slash.
    let path = &rest[rest.find('/')?..];
    let bytes: Vec<u8> = percent_decode_str(path).collect();
    Some(trove_fs::os::path_from_uri_bytes(bytes))
}

fn parse_gnome(text: &str) -> Option<(Vec<PathBuf>, bool)> {
    let mut lines = text.lines();
    let cut = match lines.next()?.trim() {
        "cut" => true,
        "copy" => false,
        _ => return None,
    };
    let paths: Vec<PathBuf> = lines.filter_map(file_uri_path).collect();
    (!paths.is_empty()).then_some((paths, cut))
}

fn parse_uri_list(text: &str) -> Vec<PathBuf> {
    text.lines().filter(|l| !l.starts_with('#')).filter_map(file_uri_path).collect()
}

fn global() -> &'static Clipboard {
    static GLOBAL: OnceLock<Clipboard> = OnceLock::new();
    GLOBAL.get_or_init(|| Clipboard::new(Box::new(system::System)))
}

pub fn copy_files(paths: &[PathBuf], cut: bool) {
    global().copy_files(paths, cut);
}

pub fn paste_files() -> Option<(Vec<PathBuf>, bool)> {
    global().paste_files()
}

/// Forget the in-process copy, e.g. after a cut was pasted.
pub fn clear() {
    global().clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct Fake {
        store: Mutex<HashMap<String, Vec<u8>>>,
        offline: bool,
    }

    impl Backend for std::sync::Arc<Fake> {
        fn offer(&self, mime: &str, data: &[u8]) -> bool {
            if self.offline {
                return false;
            }
            let mut s = self.store.lock().unwrap();
            s.clear();
            s.insert(mime.to_owned(), data.to_vec());
            true
        }
        fn types(&self) -> Option<Vec<String>> {
            (!self.offline).then(|| self.store.lock().unwrap().keys().cloned().collect())
        }
        fn read(&self, mime: &str) -> Option<Vec<u8>> {
            self.store.lock().unwrap().get(mime).cloned()
        }
    }

    fn pair(offline: bool) -> (Clipboard, std::sync::Arc<Fake>) {
        let fake = std::sync::Arc::new(Fake { offline, ..Fake::default() });
        (Clipboard::new(Box::new(fake.clone())), fake)
    }

    #[test]
    fn round_trip_through_gnome_type() {
        let (cb, fake) = pair(false);
        let paths = vec![PathBuf::from("/tmp/a b.txt"), PathBuf::from("/tmp/ü/é#.txt")];
        cb.copy_files(&paths, true);
        let payload = String::from_utf8(fake.store.lock().unwrap()[GNOME].clone()).unwrap();
        assert_eq!(payload, "cut\nfile:///tmp/a%20b.txt\nfile:///tmp/%C3%BC/%C3%A9%23.txt");
        assert_eq!(cb.paste_files(), Some((paths, true)));
    }

    #[test]
    fn reads_what_another_app_copied() {
        let (cb, fake) = pair(false);
        fake.store.lock().unwrap().insert(GNOME.into(), b"copy\nfile:///x/y%20z".to_vec());
        assert_eq!(cb.paste_files(), Some((vec![PathBuf::from("/x/y z")], false)));
        fake.store.lock().unwrap().clear();
        fake.store.lock().unwrap().insert(URI_LIST.into(), b"# c\r\nfile:///a\r\nhttp://nope\r\n".to_vec());
        fake.store.lock().unwrap().insert(KDE_CUT.into(), b"1".to_vec());
        assert_eq!(cb.paste_files(), Some((vec![PathBuf::from("/a")], true)));
    }

    #[test]
    fn plain_text_path_must_exist() {
        let d = tempfile::tempdir().unwrap();
        let (cb, fake) = pair(false);
        fake.store.lock().unwrap().insert("text/plain".into(), d.path().to_string_lossy().as_bytes().to_vec());
        assert_eq!(cb.paste_files(), Some((vec![d.path().to_path_buf()], false)));
        fake.store.lock().unwrap().insert("text/plain".into(), b"/definitely/not/here".to_vec());
        assert_eq!(cb.paste_files(), None);
    }

    #[test]
    fn local_state_when_system_is_unavailable() {
        let (cb, _) = pair(true);
        assert_eq!(cb.paste_files(), None);
        cb.copy_files(&[PathBuf::from("/a")], false);
        assert_eq!(cb.paste_files(), Some((vec![PathBuf::from("/a")], false)));
        cb.clear();
        assert_eq!(cb.paste_files(), None);
    }

    #[test]
    fn mirrored_copy_yields_to_newer_system_content() {
        let (cb, fake) = pair(false);
        cb.copy_files(&[PathBuf::from("/a")], false);
        fake.store.lock().unwrap().clear();
        fake.store.lock().unwrap().insert("text/plain".into(), b"hello".to_vec());
        assert_eq!(cb.paste_files(), None);
    }
}
