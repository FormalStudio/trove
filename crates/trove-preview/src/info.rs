use std::fs::{self, Metadata};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::mime;
use crate::preview::image_size;

#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    pub path: PathBuf,
    pub is_dir: bool,
    /// Apparent size in bytes.
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub created: Option<SystemTime>,
    pub accessed: Option<SystemTime>,
    /// `ls -l` style, e.g. `drwxr-xr-x`.
    pub mode: String,
    pub owner: String,
    pub group: String,
    pub mime: String,
    pub link_target: Option<PathBuf>,
    pub dimensions: Option<(u32, u32)>,
}

/// On a worker thread: stats and reads headers, so never call it from the UI thread directly.
pub fn info(path: PathBuf) -> flume::Receiver<Info> {
    let (tx, rx) = flume::bounded(1);
    let spawned = std::thread::Builder::new().name("trove-info".into()).spawn(move || {
        if let Some(i) = info_blocking(&path) {
            let _ = tx.send(i);
        }
    });
    drop(spawned);
    rx
}

/// Blocking form of [`info`]. `None` when the path cannot be stat'd.
pub fn info_blocking(path: &Path) -> Option<Info> {
    let lstat = fs::symlink_metadata(path).ok()?;
    let link_target = lstat.file_type().is_symlink().then(|| fs::read_link(path).ok()).flatten();
    // Size, dates and mime describe what a link points at; mode and owner describe the link.
    let target: Metadata =
        if link_target.is_some() { fs::metadata(path).unwrap_or_else(|_| lstat.clone()) } else { lstat.clone() };
    let is_dir = target.is_dir();
    let mime = mime::mime_of(path, is_dir);
    let dimensions = (!is_dir && mime.starts_with("image/")).then(|| image_size(path)).flatten();
    Some(Info {
        path: path.to_path_buf(),
        is_dir,
        size: target.len(),
        modified: target.modified().ok(),
        created: target.created().ok(),
        accessed: target.accessed().ok(),
        mode: mode_string(&lstat),
        owner: user_name(lstat.uid()),
        group: group_name(lstat.gid()),
        mime,
        link_target,
        dimensions,
    })
}

pub(crate) fn mode_string(m: &Metadata) -> String {
    let ft = m.file_type();
    let kind = if ft.is_dir() {
        'd'
    } else if ft.is_symlink() {
        'l'
    } else if ft.is_block_device() {
        'b'
    } else if ft.is_char_device() {
        'c'
    } else if ft.is_fifo() {
        'p'
    } else if ft.is_socket() {
        's'
    } else {
        '-'
    };
    let mode = m.mode();
    let bit = |mask: u32, ch: char| if mode & mask != 0 { ch } else { '-' };
    let exec = |x: u32, special: u32, on: char, off: char| match (mode & x != 0, mode & special != 0) {
        (true, true) => on,
        (false, true) => off,
        (true, false) => 'x',
        (false, false) => '-',
    };
    [
        kind,
        bit(0o400, 'r'),
        bit(0o200, 'w'),
        exec(0o100, 0o4000, 's', 'S'),
        bit(0o040, 'r'),
        bit(0o020, 'w'),
        exec(0o010, 0o2000, 's', 'S'),
        bit(0o004, 'r'),
        bit(0o002, 'w'),
        exec(0o001, 0o1000, 't', 'T'),
    ]
    .iter()
    .collect()
}

fn user_name(uid: u32) -> String {
    let mut buf = vec![0u8; 1024];
    loop {
        // SAFETY: zeroed passwd is a valid out-parameter; buf outlives the call and its length is passed.
        let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
        let mut out: *mut libc::passwd = std::ptr::null_mut();
        let rc = unsafe { libc::getpwuid_r(uid, &mut pw, buf.as_mut_ptr().cast(), buf.len(), &mut out) };
        if rc == libc::ERANGE && buf.len() < 1 << 20 {
            buf.resize(buf.len() * 4, 0);
            continue;
        }
        if rc != 0 || out.is_null() {
            return uid.to_string();
        }
        // SAFETY: on success pw_name points into buf, NUL terminated.
        return unsafe { std::ffi::CStr::from_ptr(pw.pw_name) }.to_string_lossy().into_owned();
    }
}

fn group_name(gid: u32) -> String {
    let mut buf = vec![0u8; 1024];
    loop {
        // SAFETY: as in `user_name`.
        let mut gr: libc::group = unsafe { std::mem::zeroed() };
        let mut out: *mut libc::group = std::ptr::null_mut();
        let rc = unsafe { libc::getgrgid_r(gid, &mut gr, buf.as_mut_ptr().cast(), buf.len(), &mut out) };
        if rc == libc::ERANGE && buf.len() < 1 << 20 {
            buf.resize(buf.len() * 4, 0);
            continue;
        }
        if rc != 0 || out.is_null() {
            return gid.to_string();
        }
        // SAFETY: on success gr_name points into buf, NUL terminated.
        return unsafe { std::ffi::CStr::from_ptr(gr.gr_name) }.to_string_lossy().into_owned();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn mode_strings() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f");
        fs::write(&f, "x").unwrap();
        fs::set_permissions(&f, fs::Permissions::from_mode(0o4755)).unwrap();
        assert_eq!(mode_string(&fs::metadata(&f).unwrap()), "-rwsr-xr-x");
        fs::set_permissions(&f, fs::Permissions::from_mode(0o640)).unwrap();
        assert_eq!(mode_string(&fs::metadata(&f).unwrap()), "-rw-r-----");
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o1777)).unwrap();
        assert_eq!(mode_string(&fs::metadata(dir.path()).unwrap()), "drwxrwxrwt");
    }

    #[test]
    fn info_for_file_dir_and_link() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("a.png");
        let mut bytes = Vec::new();
        image::DynamicImage::new_rgb8(7, 5)
            .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        fs::write(&png, &bytes).unwrap();
        let i = info_blocking(&png).unwrap();
        assert_eq!((i.size, i.mime.as_str(), i.dimensions), (bytes.len() as u64, "image/png", Some((7, 5))));
        assert!(i.modified.is_some() && !i.owner.is_empty() && !i.group.is_empty());
        assert_eq!(info_blocking(dir.path()).unwrap().mime, "inode/directory");
        let link = dir.path().join("l");
        symlink(&png, &link).unwrap();
        let l = info_blocking(&link).unwrap();
        assert_eq!(l.link_target.as_deref(), Some(png.as_path()));
        assert!(l.mode.starts_with('l'));
        assert_eq!(l.size, bytes.len() as u64);
        assert!(info_blocking(&dir.path().join("missing")).is_none());
    }

    #[test]
    fn channel_form_delivers() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("t.txt");
        fs::write(&f, "hi").unwrap();
        let i = info(f).recv().unwrap();
        assert_eq!(i.size, 2);
    }
}
