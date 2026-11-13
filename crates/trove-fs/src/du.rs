//! What fills a folder: a walk of everything under it, added up into a tree of sizes. The walk
//! stays on the root's file system, never follows a link, counts a hard-linked file once, and
//! measures the space a file takes on disk rather than its length.

use std::collections::HashSet;
use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::entry::FileKind;

/// The files a folder lists by name; the rest of its files become one [`NodeKind::Rest`] node,
/// so a folder of a million small files costs one node.
const NAMED_FILES: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    Dir,
    File(FileKind),
    /// The files of a folder that are too small to list by name, as one.
    Rest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub name: String,
    /// Bytes on disk of this item and everything under it.
    pub size: u64,
    /// Files and folders under it; a file is 0, a [`NodeKind::Rest`] the files it stands for.
    pub items: u64,
    pub kind: NodeKind,
    /// Largest first.
    pub children: Vec<Node>,
}

impl Node {
    pub fn is_dir(&self) -> bool {
        self.kind == NodeKind::Dir
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub items: u64,
    pub bytes: u64,
}

#[derive(Clone, Debug)]
pub enum DuEvent {
    Progress(Progress),
    Done(Arc<Node>),
    /// The root could not be read. A walk that was stopped ends without a word.
    Failed,
}

/// Walks `root` on its own threads. Raise `stop` to end it early.
pub fn scan(root: PathBuf, stop: Arc<AtomicBool>) -> flume::Receiver<DuEvent> {
    let (tx, rx) = flume::unbounded();
    let _ = std::thread::Builder::new().name("trove-du".into()).spawn(move || {
        let tree = walk(&root, &stop, |p| {
            let _ = tx.send(DuEvent::Progress(p));
        });
        match tree {
            Some(tree) => drop(tx.send(DuEvent::Done(Arc::new(tree)))),
            None if !stop.load(Relaxed) => drop(tx.send(DuEvent::Failed)),
            None => {}
        }
    });
    rx
}

/// One folder as its worker read it: its own files, and where it hangs in the tree.
struct DirRec {
    id: u32,
    parent: u32,
    name: String,
    /// The folder's own blocks plus every file in it.
    size: u64,
    /// Entries in it, folders included.
    items: u64,
    files: Vec<(String, u64)>,
    rest: (u64, u64),
}

struct Queue {
    jobs: Vec<(u32, u32, PathBuf)>,
    /// Folders queued or being read.
    pending: usize,
}

struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
    next_id: AtomicU32,
    links: Mutex<HashSet<(u64, u64)>>,
    items: AtomicU64,
    bytes: AtomicU64,
    device: Option<u64>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(unix)]
fn on_disk(md: &Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    md.blocks() * 512
}

#[cfg(not(unix))]
fn on_disk(md: &Metadata) -> u64 {
    md.len()
}

#[cfg(unix)]
fn device(md: &Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(md.dev())
}

#[cfg(not(unix))]
fn device(_: &Metadata) -> Option<u64> {
    None
}

/// The identity of a file with more than one name, to count it once.
#[cfg(unix)]
fn link_id(md: &Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    (md.nlink() > 1).then(|| (md.dev(), md.ino()))
}

#[cfg(not(unix))]
fn link_id(_: &Metadata) -> Option<(u64, u64)> {
    None
}

fn walk(root: &Path, stop: &AtomicBool, mut report: impl FnMut(Progress)) -> Option<Node> {
    let md = fs::symlink_metadata(root).ok()?;
    let name = root.file_name().map_or_else(|| root.display().to_string(), |n| n.to_string_lossy().into_owned());
    if !md.is_dir() {
        return Some(file_node(name, on_disk(&md)));
    }
    let shared = Shared {
        queue: Mutex::new(Queue { jobs: vec![(0, u32::MAX, root.to_path_buf())], pending: 1 }),
        wake: Condvar::new(),
        next_id: AtomicU32::new(1),
        links: Mutex::new(HashSet::new()),
        items: AtomicU64::new(0),
        bytes: AtomicU64::new(on_disk(&md)),
        device: device(&md),
    };
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(2, 8);
    let (tx, rx) = flume::unbounded::<DirRec>();
    let mut recs: Vec<Option<DirRec>> = Vec::new();
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let (shared, tx) = (&shared, tx.clone());
            scope.spawn(move || work(shared, stop, &tx));
        }
        drop(tx);
        let mut last = Instant::now();
        loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(rec) => {
                    let at = rec.id as usize;
                    if recs.len() <= at {
                        recs.resize_with(at + 1, || None);
                    }
                    recs[at] = Some(rec);
                }
                Err(flume::RecvTimeoutError::Timeout) => {}
                Err(flume::RecvTimeoutError::Disconnected) => break,
            }
            if last.elapsed() >= Duration::from_millis(100) {
                last = Instant::now();
                report(Progress { items: shared.items.load(Relaxed), bytes: shared.bytes.load(Relaxed) });
            }
        }
    });
    if stop.load(Relaxed) {
        return None;
    }
    report(Progress { items: shared.items.load(Relaxed), bytes: shared.bytes.load(Relaxed) });
    if let Some(Some(root_rec)) = recs.first_mut() {
        root_rec.name = name;
        root_rec.size += on_disk(&md);
    }
    assemble(recs)
}

fn work(shared: &Shared, stop: &AtomicBool, tx: &flume::Sender<DirRec>) {
    loop {
        let job = {
            let mut q = lock(&shared.queue);
            loop {
                if stop.load(Relaxed) || q.pending == 0 {
                    shared.wake.notify_all();
                    return;
                }
                if let Some(job) = q.jobs.pop() {
                    break job;
                }
                q = shared.wake.wait(q).unwrap_or_else(|e| e.into_inner());
            }
        };
        let (id, parent, path) = job;
        let (rec, found) = read_dir(shared, id, parent, &path);
        let _ = tx.send(rec);
        let mut q = lock(&shared.queue);
        q.pending = q.pending + found.len() - 1;
        q.jobs.extend(found);
        drop(q);
        shared.wake.notify_all();
    }
}

fn read_dir(shared: &Shared, id: u32, parent: u32, path: &Path) -> (DirRec, Vec<(u32, u32, PathBuf)>) {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut rec = DirRec { id, parent, name, size: 0, items: 0, files: Vec::new(), rest: (0, 0) };
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(path) else { return (rec, found) };
    for entry in entries.filter_map(Result::ok) {
        let Ok(md) = entry.metadata() else { continue };
        rec.items += 1;
        let size = on_disk(&md);
        if md.is_dir() {
            // Another file system mounted here is not this one's space.
            if device(&md) != shared.device {
                continue;
            }
            rec.size += size;
            found.push((shared.next_id.fetch_add(1, Relaxed), id, entry.path()));
        } else {
            if let Some(link) = link_id(&md)
                && !lock(&shared.links).insert(link)
            {
                continue;
            }
            rec.size += size;
            rec.files.push((entry.file_name().to_string_lossy().into_owned(), size));
        }
    }
    shared.items.fetch_add(rec.items, Relaxed);
    shared.bytes.fetch_add(rec.size, Relaxed);
    if rec.files.len() > NAMED_FILES {
        rec.files.sort_unstable_by(|a, b| b.1.cmp(&a.1));
        let rest = rec.files.split_off(NAMED_FILES);
        rec.rest = (rest.len() as u64, rest.iter().map(|f| f.1).sum());
    }
    (rec, found)
}

fn file_node(name: String, size: u64) -> Node {
    let kind = NodeKind::File(FileKind::from_name(&name));
    Node { name, size, items: 0, kind, children: Vec::new() }
}

/// A folder's id is always above its parent's, so going down the ids every folder is whole
/// before the one holding it.
fn assemble(recs: Vec<Option<DirRec>>) -> Option<Node> {
    let mut kids: Vec<Vec<Node>> = Vec::new();
    kids.resize_with(recs.len(), Vec::new);
    for rec in recs.into_iter().rev().flatten() {
        let mut children = std::mem::take(&mut kids[rec.id as usize]);
        let mut size = rec.size;
        let mut items = rec.items;
        for c in &children {
            size += c.size;
            items += c.items;
        }
        children.extend(rec.files.into_iter().map(|(name, size)| file_node(name, size)));
        if rec.rest.0 > 0 {
            let name = format!("{} smaller files", rec.rest.0);
            children.push(Node {
                name,
                size: rec.rest.1,
                items: rec.rest.0,
                kind: NodeKind::Rest,
                children: Vec::new(),
            });
        }
        children.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.name.cmp(&b.name)));
        let node = Node { name: rec.name, size, items, kind: NodeKind::Dir, children };
        match kids.get_mut(rec.parent as usize) {
            Some(siblings) => siblings.push(node),
            None => return Some(node),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(root: &Path) -> Node {
        walk(root, &AtomicBool::new(false), |_| {}).expect("tree")
    }

    fn child<'a>(n: &'a Node, name: &str) -> &'a Node {
        n.children.iter().find(|c| c.name == name).expect("child")
    }

    #[test]
    fn adds_up_folders_largest_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        let d = dir.path();
        fs::create_dir_all(d.join("big/deep")).expect("mkdir");
        fs::create_dir_all(d.join("small")).expect("mkdir");
        fs::write(d.join("big/deep/a.mp4"), vec![1u8; 300_000]).expect("write");
        fs::write(d.join("big/b.png"), vec![1u8; 100_000]).expect("write");
        fs::write(d.join("small/c.txt"), vec![1u8; 10_000]).expect("write");
        let t = tree(d);
        assert_eq!(t.kind, NodeKind::Dir);
        assert_eq!(t.items, 6);
        assert_eq!(t.children[0].name, "big");
        assert_eq!(t.children[1].name, "small");
        let big = child(&t, "big");
        assert_eq!(big.children[0].name, "deep");
        assert_eq!(child(big, "b.png").kind, NodeKind::File(FileKind::Image));
        assert!(big.size >= 400_000 && big.size < 500_000, "{}", big.size);
        assert_eq!(t.size, t.children.iter().map(|c| c.size).sum::<u64>() + on_disk(&fs::metadata(d).expect("md")));
    }

    #[test]
    fn small_files_fold_into_one_node() {
        let dir = tempfile::tempdir().expect("tempdir");
        for i in 0..NAMED_FILES + 10 {
            fs::write(dir.path().join(format!("f{i}")), vec![1u8; 5000 + i * 4096]).expect("write");
        }
        let t = tree(dir.path());
        assert_eq!(t.children.len(), NAMED_FILES + 1);
        let rest = t.children.iter().find(|c| c.kind == NodeKind::Rest).expect("rest");
        assert_eq!(rest.items, 10);
        assert_eq!(rest.name, "10 smaller files");
        assert_eq!(t.items, (NAMED_FILES + 10) as u64);
    }

    #[cfg(unix)]
    #[test]
    fn links_are_not_followed_and_hard_links_count_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let d = dir.path();
        fs::create_dir(d.join("real")).expect("mkdir");
        fs::write(d.join("real/data"), vec![1u8; 200_000]).expect("write");
        fs::hard_link(d.join("real/data"), d.join("real/again")).expect("link");
        std::os::unix::fs::symlink(d.join("real"), d.join("alias")).expect("symlink");
        std::os::unix::fs::symlink("..", d.join("real/up")).expect("symlink");
        let t = tree(d);
        assert!(t.size >= 200_000 && t.size < 300_000, "{}", t.size);
        assert!(!child(&t, "alias").is_dir());
    }

    #[test]
    fn a_stopped_walk_gives_nothing_and_a_file_is_itself() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("one"), vec![1u8; 9000]).expect("write");
        assert!(walk(dir.path(), &AtomicBool::new(true), |_| {}).is_none());
        let one = tree(&dir.path().join("one"));
        assert_eq!(one.kind, NodeKind::File(FileKind::Unknown));
        assert!(one.children.is_empty());
        let rx = scan(dir.path().to_path_buf(), Arc::new(AtomicBool::new(false)));
        let done = rx.iter().find_map(|e| match e {
            DuEvent::Done(t) => Some(t),
            _ => None,
        });
        assert_eq!(done.map(|t| t.items), Some(1));
        let gone = scan(dir.path().join("missing"), Arc::new(AtomicBool::new(false)));
        assert!(matches!(gone.recv(), Ok(DuEvent::Failed)));
    }
}
