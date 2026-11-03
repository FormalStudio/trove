use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::text;

/// By name first; the file's first bytes only when the name says nothing.
pub(crate) fn mime_of(path: &Path, is_dir: bool) -> String {
    if is_dir {
        return "inode/directory".to_owned();
    }
    if let Some(m) = by_name(path) {
        return m;
    }
    let mut head = [0u8; 8192];
    let n = File::open(path).and_then(|mut f| f.read(&mut head)).unwrap_or(0);
    let head = &head[..n];
    if let Some(t) = infer::get(head) {
        return t.mime_type().to_owned();
    }
    if n == 0 || text::decode(head, false).is_some() {
        "text/plain".to_owned()
    } else {
        "application/octet-stream".to_owned()
    }
}

fn by_name(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let ext = trove_fs::entry::extension_of(name).to_ascii_lowercase();
    // mime_guess maps `ts` to MPEG transport streams.
    if ext == "ts" {
        return Some("text/typescript".to_owned());
    }
    mime_guess::from_path(path).first().map(|m| m.essence_str().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn by_name_wins_and_sniff_fills_in() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(mime_of(dir.path(), true), "inode/directory");
        assert_eq!(mime_of(Path::new("/nonexistent/a.png"), false), "image/png");
        assert_eq!(mime_of(Path::new("/nonexistent/IMG_1.HEIC"), false), "image/heic");
        assert_eq!(mime_of(Path::new("/nonexistent/a.heif"), false), "image/heif");
        assert_eq!(mime_of(Path::new("/nonexistent/a.ts"), false), "text/typescript");
        let png = dir.path().join("noext");
        std::fs::write(&png, [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0]).unwrap();
        assert_eq!(mime_of(&png, false), "image/png");
        let txt = dir.path().join("plain");
        std::fs::write(&txt, "hello\n").unwrap();
        assert_eq!(mime_of(&txt, false), "text/plain");
        let bin = dir.path().join("blob");
        std::fs::write(&bin, [0u8, 1, 2, 3, 0xff, 0xfe]).unwrap();
        assert_eq!(mime_of(&bin, false), "application/octet-stream");
    }
}
