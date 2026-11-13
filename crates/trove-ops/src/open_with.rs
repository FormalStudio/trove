//! Opening files: MIME types, the apps that handle them, the default one, and launching. The
//! launching itself is per OS: desktop entries on Linux, `open` on macOS, the shell on Windows.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;
use std::sync::{Arc, Mutex};

pub use crate::desktop::{AppInfo, Env, Registry};

const SNIFF_BYTES: usize = 8192;

static REGISTRY: Mutex<Option<Arc<Registry>>> = Mutex::new(None);

/// The parsed desktop entries, loaded on first use and again whenever an applications
/// directory or a `mimeapps.list` changed on disk.
pub fn registry() -> Arc<Registry> {
    let mut slot = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(reg) = slot.as_ref()
        && !reg.is_stale()
    {
        return Arc::clone(reg);
    }
    let reg = Arc::new(Registry::load(Env::from_env()));
    *slot = Some(Arc::clone(&reg));
    reg
}

/// Drops the cache so the next call reads the disk again.
pub fn refresh() {
    *REGISTRY.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// By name first; only a file whose name says nothing has its first 8 KiB sniffed.
pub fn mime_of(path: &Path) -> String {
    if fs::metadata(path).is_ok_and(|m| m.is_dir()) {
        return "inode/directory".into();
    }
    if let Some(m) = mime_guess::from_path(path).first_raw() {
        return m.to_owned();
    }
    sniff(path).unwrap_or_else(|| "application/octet-stream".into())
}

fn sniff(path: &Path) -> Option<String> {
    let mut buf = vec![0u8; SNIFF_BYTES];
    let mut file = File::open(path).ok()?;
    let mut len = 0;
    while len < buf.len() {
        match file.read(&mut buf[len..]) {
            Ok(0) => break,
            Ok(n) => len += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
    buf.truncate(len);
    if let Some(kind) = infer::get(&buf) {
        return Some(kind.mime_type().to_owned());
    }
    let text_like = !buf.contains(&0)
        && match std::str::from_utf8(&buf) {
            Ok(_) => true,
            // A multi-byte character cut off by the 8 KiB limit is still text.
            Err(e) => e.error_len().is_none(),
        };
    text_like.then(|| "text/plain".to_owned())
}

#[cfg(not(any(target_os = "macos", windows)))]
#[path = "open_with/xdg.rs"]
mod launch;
#[cfg(target_os = "macos")]
#[path = "open_with/macos.rs"]
mod launch;
#[cfg(windows)]
#[path = "open_with/windows.rs"]
mod launch;

pub use launch::{apps_for, open, open_terminal, open_with, set_default};

#[cfg(test)]
mod tests;
