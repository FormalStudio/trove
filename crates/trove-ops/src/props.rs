//! What the Properties sheet reads that a listing does not: the space a folder's contents take,
//! and whether this process may change a file's permissions.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// Apparent size of every file, links counted as themselves.
    pub bytes: u64,
    /// Files and folders inside the folders given, not the folders themselves.
    pub items: u64,
}

/// The total size of `paths`, folders walked to the bottom without following links. Stops early,
/// with what it has, once `stop` is set.
pub fn usage(paths: &[PathBuf], stop: &AtomicBool) -> Usage {
    let mut total = Usage::default();
    let mut stack: Vec<PathBuf> = Vec::new();
    for path in paths {
        let Ok(md) = fs::symlink_metadata(path) else { continue };
        if md.is_dir() {
            stack.push(path.clone());
        } else {
            total.bytes += md.len();
        }
    }
    while let Some(dir) = stack.pop() {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for entry in entries.filter_map(Result::ok) {
            let Ok(md) = entry.metadata() else { continue };
            total.items += 1;
            if md.is_dir() {
                stack.push(entry.path());
            } else {
                total.bytes += md.len();
            }
        }
    }
    total
}

/// Whether chmod(2) on a file owned by `uid` can succeed for this process: it owns the file,
/// or it is root.
pub fn may_change_mode(uid: u32) -> bool {
    // SAFETY: geteuid has no preconditions and cannot fail.
    let me = unsafe { libc::geteuid() };
    me == 0 || me == uid
}

/// The owner's uid of `path`, links followed.
pub fn owner_of(path: &std::path::Path) -> Option<u32> {
    fs::metadata(path).ok().map(|m| m.uid())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_walks_folders_and_adds_files() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        fs::create_dir_all(d.join("a/b")).unwrap();
        fs::write(d.join("a/one"), [0u8; 10]).unwrap();
        fs::write(d.join("a/b/two"), [0u8; 5]).unwrap();
        fs::write(d.join("three"), [0u8; 7]).unwrap();
        let u = usage(&[d.join("a"), d.join("three")], &AtomicBool::new(false));
        assert_eq!(u, Usage { bytes: 22, items: 3 });
        let stopped = usage(&[d.join("a")], &AtomicBool::new(true));
        assert_eq!(stopped.items, 0);
        assert!(may_change_mode(owner_of(d).unwrap_or(u32::MAX)));
    }
}
