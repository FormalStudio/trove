use std::collections::HashSet;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};

use ignore::{DirEntry, ParallelVisitor, ParallelVisitorBuilder, WalkBuilder, WalkState};

use crate::index::{Index, RawEntry};
use crate::lock;
use crate::mounts::{Mount, is_slow, mount_of, skip_points};
use crate::text::is_hidden_name;

/// The design's default exclusions. One component matches a folder of that name anywhere; more
/// than one matches a folder whose path ends with them.
pub(crate) const EXCLUDED: &[&str] = &[
    ".cache",
    "node_modules",
    ".git",
    "target",
    ".local/share/Trash",
    ".cargo/registry",
    ".rustup",
    ".npm",
    ".bun",
    "__pycache__",
];

/// What a walk refuses to enter below its root.
pub(crate) struct Prune {
    pub skip: HashSet<PathBuf>,
    pub allow_hidden: bool,
}

impl Prune {
    pub fn new(root: &Path, mounts: &[Mount], allow_hidden: bool) -> Prune {
        Prune { skip: skip_points(root, mounts), allow_hidden }
    }

    pub fn keep(&self, e: &DirEntry) -> bool {
        if e.depth() == 0 {
            return true;
        }
        let name = e.file_name().as_bytes();
        if !self.allow_hidden && is_hidden_name(name) {
            return false;
        }
        if !e.file_type().is_some_and(|t| t.is_dir()) {
            return true;
        }
        !excluded(e.path(), name) && !self.skip.contains(e.path())
    }
}

pub(crate) fn excluded(path: &Path, name: &[u8]) -> bool {
    EXCLUDED.iter().any(|x| if x.contains('/') { path.ends_with(x) } else { name == x.as_bytes() })
}

pub(crate) fn walker(root: &Path, prune: Prune, max_depth: Option<usize>) -> ignore::WalkParallel {
    let prune = Arc::new(prune);
    WalkBuilder::new(root)
        .standard_filters(false)
        .follow_links(false)
        .same_file_system(false)
        .max_depth(max_depth)
        .threads(0)
        .filter_entry(move |e| prune.keep(e))
        .build_parallel()
}

/// Nice 19 and the idle I/O class for the calling thread. Both are per thread on Linux and
/// inherited by threads it spawns, which is how the walker's pool gets them too.
pub(crate) fn lower_priority() {
    #[cfg(target_os = "linux")]
    // SAFETY: plain syscalls on the calling thread with integer arguments.
    unsafe {
        let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
        libc::setpriority(libc::PRIO_PROCESS, tid, 19);
        const IOPRIO_WHO_PROCESS: libc::c_int = 1;
        const IOPRIO_CLASS_IDLE: libc::c_int = 3;
        const IOPRIO_CLASS_SHIFT: libc::c_int = 13;
        libc::syscall(libc::SYS_ioprio_set, IOPRIO_WHO_PROCESS, 0, IOPRIO_CLASS_IDLE << IOPRIO_CLASS_SHIFT);
    }
}

pub(crate) fn mtime_of(md: &std::fs::Metadata) -> u32 {
    u32::try_from(md.mtime().max(0)).unwrap_or(u32::MAX)
}

/// Walks `root` for the index. Fails when the root itself is on a network or pseudo file
/// system (never indexed) or when `stop` is raised part way.
pub(crate) fn build_index(root: &Path, scanned: &AtomicU64, stop: &AtomicBool) -> io::Result<Index> {
    let mounts = crate::mounts::read_mounts();
    if mount_of(root, &mounts).is_some_and(is_slow) {
        return Err(io::Error::other("the root is on a network file system"));
    }
    let built_at = crate::now_secs();
    let out = Mutex::new(Vec::new());
    let mut b = Collect { out: &out, scanned, stop };
    walker(root, Prune::new(root, &mounts, true), None).visit(&mut b);
    if stop.load(Relaxed) {
        return Err(io::Error::other("stopped"));
    }
    let parts = std::mem::take(&mut *lock(&out));
    let mut raw = Vec::with_capacity(parts.iter().map(Vec::len).sum());
    for p in parts {
        raw.extend(p);
    }
    Ok(Index::from_raw(root.to_path_buf(), raw, built_at))
}

struct Collect<'a> {
    out: &'a Mutex<Vec<Vec<RawEntry>>>,
    scanned: &'a AtomicU64,
    stop: &'a AtomicBool,
}

impl<'s> ParallelVisitorBuilder<'s> for Collect<'s> {
    fn build(&mut self) -> Box<dyn ParallelVisitor + 's> {
        Box::new(Visitor { buf: Vec::new(), out: self.out, scanned: self.scanned, stop: self.stop })
    }
}

struct Visitor<'a> {
    buf: Vec<RawEntry>,
    out: &'a Mutex<Vec<Vec<RawEntry>>>,
    scanned: &'a AtomicU64,
    stop: &'a AtomicBool,
}

impl ParallelVisitor for Visitor<'_> {
    fn visit(&mut self, e: Result<DirEntry, ignore::Error>) -> WalkState {
        if self.stop.load(Relaxed) {
            return WalkState::Quit;
        }
        let Ok(e) = e else { return WalkState::Continue };
        if e.depth() == 0 {
            return WalkState::Continue;
        }
        let is_dir = e.file_type().is_some_and(|t| t.is_dir());
        let (mtime, size) = e.metadata().map_or((0, 0), |md| (mtime_of(&md), md.len()));
        self.buf.push(RawEntry { path: e.into_path(), is_dir, mtime, size });
        self.scanned.fetch_add(1, Relaxed);
        WalkState::Continue
    }
}

impl Drop for Visitor<'_> {
    fn drop(&mut self) {
        lock(self.out).push(std::mem::take(&mut self.buf));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[cfg(target_os = "linux")]
    #[test]
    fn idle_priority_reaches_spawned_threads() {
        let (nice, io) = std::thread::spawn(|| {
            lower_priority();
            std::thread::spawn(|| {
                // SAFETY: plain syscalls on the calling thread.
                unsafe {
                    let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
                    (libc::getpriority(libc::PRIO_PROCESS, tid), libc::syscall(libc::SYS_ioprio_get, 1, 0))
                }
            })
            .join()
            .unwrap()
        })
        .join()
        .unwrap();
        assert_eq!(nice, 19);
        assert_eq!(io >> 13, 3);
    }

    #[test]
    fn exclusions_and_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for d in ["a/b", "node_modules/x", "p/.git/objects", "p/target/debug", ".local/share/Trash/files", ".config"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        for f in ["a/b/keep.txt", "node_modules/x/no.js", "p/.git/objects/no", "p/target/debug/no", ".config/yes.conf"]
        {
            fs::write(root.join(f), b"x").unwrap();
        }
        std::os::unix::fs::symlink("..", root.join("a/b/loop")).unwrap();
        std::os::unix::fs::symlink(root, root.join("a/self")).unwrap();
        let ix = build_index(root, &AtomicU64::new(0), &AtomicBool::new(false)).unwrap();
        let mut paths: Vec<String> = (1..ix.len() as u32)
            .map(|i| ix.path_of(i).strip_prefix(root).unwrap().to_string_lossy().into_owned())
            .collect();
        paths.sort();
        assert_eq!(
            paths,
            [
                ".config",
                ".config/yes.conf",
                ".local",
                ".local/share",
                "a",
                "a/b",
                "a/b/keep.txt",
                "a/b/loop",
                "a/self",
                "p"
            ]
        );
        let l = ix.lookup(&root.join("a/b/loop")).unwrap();
        assert!(!ix.is_dir(l));
        let k = ix.lookup(&root.join("a/b/keep.txt")).unwrap();
        assert_eq!(ix.size[k as usize], 1);
        assert!(ix.mtime[k as usize] > 0);
    }
}
