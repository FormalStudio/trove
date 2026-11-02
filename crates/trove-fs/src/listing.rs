use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use inotify::{EventMask, Inotify, WatchMask};

use crate::entry::{Entry, EntryId, EntryKind, Meta};
use crate::volumes::network_here;

const FIRST_CHUNK: usize = 256;
const MAX_CHUNK: usize = 2048;
/// A slow readdir still hands over what it has this often.
const FLUSH_AFTER: Duration = Duration::from_millis(50);
const SLOW_AFTER: Duration = Duration::from_secs(1);
const COALESCE: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ListOptions {
    pub show_hidden: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListEvent {
    /// Names and d_type from readdir, in chunks of at most 2048, first chunk as soon as it exists.
    Entries(Vec<Entry>),
    /// Metadata for entries already sent, in chunks, stat'd on a small worker pool.
    Meta(Vec<(EntryId, Meta)>),
    /// readdir finished: the full name set is known.
    Listed {
        total: usize,
    },
    /// Every entry has metadata.
    Complete,
    /// The directory is slow to answer (no first chunk after 1 s). Still running.
    Slow,
    Error(ListError),
    /// Live changes after Listed, from the watcher (local file systems only).
    Changed(Vec<Change>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Added(Entry),
    Removed(EntryId),
    Updated(EntryId, Meta),
    Renamed { from: EntryId, to: Entry },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListError {
    NotFound,
    PermissionDenied,
    NotADirectory,
    Io(String),
}

impl ListError {
    fn from_io(e: &io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::NotFound => Self::NotFound,
            io::ErrorKind::PermissionDenied => Self::PermissionDenied,
            io::ErrorKind::NotADirectory => Self::NotADirectory,
            _ => Self::Io(e.to_string()),
        }
    }
}

/// Dropping a `Listing` stops its threads and ends the watch.
pub struct Listing {
    pub events: flume::Receiver<ListEvent>,
    _cancel: CancelOnDrop,
}

pub fn list(dir: PathBuf, _opts: ListOptions) -> Listing {
    let (tx, rx) = flume::unbounded();
    let cancel = Arc::new(Cancel::new());
    let ctx = Ctx { tx, cancel: cancel.clone() };
    let spawned = std::thread::Builder::new().name("trove-list".into()).spawn({
        let ctx = ctx.clone();
        move || run(&dir, &ctx)
    });
    if let Err(e) = spawned {
        ctx.send(ListEvent::Error(ListError::Io(e.to_string())));
    }
    Listing { events: rx, _cancel: CancelOnDrop(cancel) }
}

struct Cancel {
    flag: AtomicBool,
    /// Wakes the watcher's poll so a dropped listing releases its inotify instance at once.
    wake: Option<OwnedFd>,
}

impl Cancel {
    fn new() -> Self {
        // SAFETY: eventfd takes plain integers; a valid result is an fd we own.
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        // SAFETY: `fd` is a fresh descriptor not owned elsewhere.
        let wake = (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd) });
        Self { flag: AtomicBool::new(false), wake }
    }

    fn is_set(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }

    fn trigger(&self) {
        self.flag.store(true, Ordering::Relaxed);
        if let Some(fd) = &self.wake {
            let one = 1u64;
            // SAFETY: writing 8 bytes from a live u64 to an eventfd we own.
            unsafe { libc::write(fd.as_raw_fd(), (&raw const one).cast(), 8) };
        }
    }
}

struct CancelOnDrop(Arc<Cancel>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.trigger();
    }
}

#[derive(Clone)]
struct Ctx {
    tx: flume::Sender<ListEvent>,
    cancel: Arc<Cancel>,
}

impl Ctx {
    /// False once the listing is cancelled or nobody is listening any more.
    fn send(&self, ev: ListEvent) -> bool {
        if self.cancel.is_set() {
            return false;
        }
        if self.tx.send(ev).is_err() {
            self.cancel.trigger();
            return false;
        }
        true
    }
}

fn kind_of(ft: fs::FileType) -> EntryKind {
    if ft.is_dir() {
        EntryKind::Dir
    } else if ft.is_symlink() {
        EntryKind::Symlink { to_dir: false, broken: false }
    } else if ft.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    }
}

/// lstat for the entry, stat to resolve a symlink. Returns the d_type-style kind and the Meta.
fn stat(path: &Path) -> io::Result<(EntryKind, Meta)> {
    let md = fs::symlink_metadata(path)?;
    let raw = kind_of(md.file_type());
    let kind = match raw {
        EntryKind::Symlink { .. } => match fs::metadata(path) {
            Ok(target) => EntryKind::Symlink { to_dir: target.is_dir(), broken: false },
            Err(_) => EntryKind::Symlink { to_dir: false, broken: true },
        },
        k => k,
    };
    let meta = Meta {
        size: md.len(),
        modified: md.modified().ok(),
        created: md.created().ok(),
        accessed: md.accessed().ok(),
        mode: md.mode() & 0o7777,
        uid: md.uid(),
        gid: md.gid(),
        kind,
        child_count: None,
    };
    Ok((raw, meta))
}

fn name_string(name: &OsStr) -> String {
    name.to_string_lossy().into_owned()
}

fn make_entry(dir: &Path, name: &OsStr) -> io::Result<Entry> {
    let (raw, meta) = stat(&dir.join(name))?;
    let mut entry = Entry::new(&name_string(name), raw);
    entry.meta = Some(meta);
    Ok(entry)
}

fn run(dir: &Path, ctx: &Ctx) {
    // Dropped (or sent) once the first chunk has gone out; if it times out first, the timer says Slow.
    let (first_tx, first_rx) = flume::bounded::<()>(0);
    let _ = std::thread::Builder::new().name("trove-list-slow".into()).spawn({
        let ctx = ctx.clone();
        move || {
            if matches!(first_rx.recv_timeout(SLOW_AFTER), Err(flume::RecvTimeoutError::Timeout)) {
                ctx.send(ListEvent::Slow);
            }
        }
    });
    let mut first_guard = Some(first_tx);

    let (network, mounts) = network_here(dir);
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) => {
            ctx.send(ListEvent::Error(ListError::from_io(&e)));
            return;
        }
    };
    // Armed before readdir so nothing that happens during the listing is missed.
    let watcher = if network { None } else { Watcher::new(dir) };

    let mut pool = Pool::new(dir, ctx, network, mounts);
    let mut known: Vec<EntryId> = Vec::new();
    let mut chunk: Vec<Entry> = Vec::with_capacity(FIRST_CHUNK);
    let mut limit = FIRST_CHUNK;
    let mut chunk_started = Instant::now();
    let mut total = 0usize;

    for item in rd {
        if ctx.cancel.is_set() {
            return;
        }
        let de = match item {
            Ok(de) => de,
            Err(e) => {
                ctx.send(ListEvent::Error(ListError::from_io(&e)));
                ctx.cancel.trigger();
                return;
            }
        };
        let kind = de.file_type().map_or(EntryKind::Other, kind_of);
        let entry = Entry::new(&name_string(&de.file_name()), kind);
        if watcher.is_some() {
            known.push(entry.id);
        }
        chunk.push(entry);
        total += 1;
        let due = chunk.len() >= limit || (chunk.len().is_multiple_of(64) && chunk_started.elapsed() >= FLUSH_AFTER);
        if due {
            if !flush(&mut chunk, ctx, &mut pool) {
                return;
            }
            first_guard = None;
            limit = MAX_CHUNK;
            chunk_started = Instant::now();
        }
    }
    if !chunk.is_empty() && !flush(&mut chunk, ctx, &mut pool) {
        return;
    }
    drop(first_guard);
    if !ctx.send(ListEvent::Listed { total }) {
        return;
    }
    pool.finish();
    if !ctx.send(ListEvent::Complete) {
        return;
    }
    if let Some(watcher) = watcher {
        watcher.run(dir, known.into_iter().collect(), ctx);
    }
}

fn flush(chunk: &mut Vec<Entry>, ctx: &Ctx, pool: &mut Pool) -> bool {
    let entries = std::mem::take(chunk);
    let jobs = pool.jobs_for(&entries);
    if !ctx.send(ListEvent::Entries(entries)) {
        return false;
    }
    pool.submit(jobs);
    true
}

type Job = Vec<(EntryId, Arc<str>)>;

/// Stats run on up to 8 threads locally and 2 on network file systems, started as work arrives so
/// a small directory costs one thread.
struct Pool {
    dir: Arc<PathBuf>,
    ctx: Ctx,
    jobs_tx: Option<flume::Sender<Job>>,
    jobs_rx: flume::Receiver<Job>,
    workers: Vec<JoinHandle<()>>,
    max_workers: usize,
    job_size: usize,
    /// Network mount points in the folder, which keep their readdir kind and get no stat.
    unstatted: HashSet<String>,
}

impl Pool {
    fn new(dir: &Path, ctx: &Ctx, network: bool, unstatted: HashSet<String>) -> Self {
        let (jobs_tx, jobs_rx) = flume::unbounded();
        Self {
            dir: Arc::new(dir.to_path_buf()),
            ctx: ctx.clone(),
            jobs_tx: Some(jobs_tx),
            jobs_rx,
            workers: Vec::new(),
            max_workers: if network { 2 } else { 8 },
            job_size: if network { 8 } else { 256 },
            unstatted,
        }
    }

    fn jobs_for(&self, entries: &[Entry]) -> Vec<Job> {
        let statted: Vec<&Entry> = entries.iter().filter(|e| !self.unstatted.contains(&*e.name)).collect();
        statted.chunks(self.job_size).map(|c| c.iter().map(|e| (e.id, e.name.clone())).collect()).collect()
    }

    fn submit(&mut self, jobs: Vec<Job>) {
        for job in jobs {
            if let Some(tx) = &self.jobs_tx {
                let _ = tx.send(job);
            }
            if self.workers.len() < self.max_workers {
                self.spawn_worker();
            }
        }
    }

    fn spawn_worker(&mut self) {
        let (rx, ctx, dir) = (self.jobs_rx.clone(), self.ctx.clone(), self.dir.clone());
        let handle = std::thread::Builder::new().name("trove-stat".into()).spawn(move || {
            while let Ok(job) = rx.recv() {
                let mut metas = Vec::with_capacity(job.len());
                for (id, name) in job {
                    if ctx.cancel.is_set() {
                        return;
                    }
                    // A name that vanished since readdir has no meta; the watcher reports its removal.
                    if let Ok((_, meta)) = stat(&dir.join(&*name)) {
                        metas.push((id, meta));
                    }
                }
                if !metas.is_empty() && !ctx.send(ListEvent::Meta(metas)) {
                    return;
                }
            }
        });
        if let Ok(h) = handle {
            self.workers.push(h);
        }
    }

    fn finish(&mut self) {
        self.jobs_tx = None;
        // No worker could start: stat inline rather than leave entries without meta.
        if self.workers.is_empty() {
            while let Ok(job) = self.jobs_rx.try_recv() {
                let metas: Vec<_> = job
                    .into_iter()
                    .filter_map(|(id, n)| stat(&self.dir.join(&*n)).ok().map(|(_, m)| (id, m)))
                    .collect();
                if !self.ctx.send(ListEvent::Meta(metas)) {
                    return;
                }
            }
        }
        for h in self.workers.drain(..) {
            let _ = h.join();
        }
    }
}

struct Watcher {
    ino: Inotify,
}

impl Watcher {
    fn new(dir: &Path) -> Option<Self> {
        let ino = Inotify::init().ok()?;
        let mask = WatchMask::CREATE
            | WatchMask::DELETE
            | WatchMask::MODIFY
            | WatchMask::ATTRIB
            | WatchMask::CLOSE_WRITE
            | WatchMask::MOVED_FROM
            | WatchMask::MOVED_TO
            | WatchMask::DELETE_SELF
            | WatchMask::MOVE_SELF
            | WatchMask::ONLYDIR;
        ino.watches().add(dir, mask).ok()?;
        Some(Self { ino })
    }

    fn run(mut self, dir: &Path, mut known: HashSet<EntryId>, ctx: &Ctx) {
        let ino_fd = self.ino.as_raw_fd();
        let wake_fd = ctx.cancel.wake.as_ref().map(AsRawFd::as_raw_fd);
        let mut buf = vec![0u8; 32 * 1024];
        let mut batch = Batch::default();
        let mut deadline: Option<Instant> = None;
        loop {
            let timeout = match (deadline, wake_fd) {
                (Some(d), _) => d.saturating_duration_since(Instant::now()).as_millis().min(i32::MAX as u128) as i32,
                (None, Some(_)) => -1,
                (None, None) => 1000,
            };
            if !wait(ino_fd, wake_fd, timeout) || ctx.cancel.is_set() {
                return;
            }
            loop {
                match self.ino.read_events(&mut buf) {
                    Ok(events) => {
                        for ev in events {
                            batch.add(ev.mask, ev.name, ev.cookie);
                            deadline.get_or_insert_with(|| Instant::now() + COALESCE);
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => return,
                }
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                deadline = None;
                let gone = batch.dir_gone;
                let changes = std::mem::take(&mut batch).resolve(dir, &mut known);
                for part in changes.chunks(MAX_CHUNK) {
                    if !ctx.send(ListEvent::Changed(part.to_vec())) {
                        return;
                    }
                }
                if gone {
                    ctx.send(ListEvent::Error(ListError::NotFound));
                    return;
                }
            }
        }
    }
}

/// Blocks until the inotify fd is readable, the cancel fd fires or the timeout passes. False on a
/// poll error.
fn wait(ino_fd: RawFd, wake_fd: Option<RawFd>, timeout_ms: i32) -> bool {
    let mut fds = [
        libc::pollfd { fd: ino_fd, events: libc::POLLIN, revents: 0 },
        libc::pollfd { fd: wake_fd.unwrap_or(-1), events: libc::POLLIN, revents: 0 },
    ];
    loop {
        // SAFETY: `fds` is a valid array of two pollfds; a negative fd is ignored by poll.
        let r = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout_ms) };
        if r >= 0 {
            return true;
        }
        if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return false;
        }
    }
}

#[derive(Default)]
struct Batch {
    touched: Vec<OsString>,
    seen: HashSet<OsString>,
    moved_from: Vec<(u32, OsString)>,
    renames: Vec<(OsString, OsString)>,
    overflow: bool,
    dir_gone: bool,
}

impl Batch {
    fn touch(&mut self, name: &OsStr) {
        if self.seen.insert(name.to_os_string()) {
            self.touched.push(name.to_os_string());
        }
    }

    fn add(&mut self, mask: EventMask, name: Option<&OsStr>, cookie: u32) {
        if mask.contains(EventMask::Q_OVERFLOW) {
            self.overflow = true;
        }
        if mask.intersects(EventMask::DELETE_SELF | EventMask::MOVE_SELF | EventMask::UNMOUNT) {
            self.dir_gone = true;
        }
        let Some(name) = name else { return };
        self.touch(name);
        if mask.contains(EventMask::MOVED_FROM) {
            self.moved_from.push((cookie, name.to_os_string()));
        } else if mask.contains(EventMask::MOVED_TO)
            && let Some(i) = self.moved_from.iter().position(|(c, _)| *c == cookie)
        {
            let (_, from) = self.moved_from.swap_remove(i);
            self.renames.push((from, name.to_os_string()));
        }
    }

    /// Settles the batch against the directory as it is now, so a create followed by a delete
    /// inside the window reports nothing.
    fn resolve(mut self, dir: &Path, known: &mut HashSet<EntryId>) -> Vec<Change> {
        let mut changes = Vec::new();
        if self.overflow {
            self.rescan(dir, known, &mut changes);
        }
        let mut handled: HashSet<OsString> = HashSet::new();
        for (from, to) in &self.renames {
            let (from_id, to_id) = (EntryId::of(&name_string(from)), EntryId::of(&name_string(to)));
            if !known.contains(&from_id) || known.contains(&to_id) || handled.contains(from) || handled.contains(to) {
                continue;
            }
            let from_gone =
                matches!(fs::symlink_metadata(dir.join(from)), Err(e) if e.kind() == io::ErrorKind::NotFound);
            if let (true, Ok(entry)) = (from_gone, make_entry(dir, to)) {
                known.remove(&from_id);
                known.insert(entry.id);
                changes.push(Change::Renamed { from: from_id, to: entry });
                handled.insert(from.clone());
                handled.insert(to.clone());
            }
        }
        for name in &self.touched {
            if handled.contains(name) {
                continue;
            }
            let id = EntryId::of(&name_string(name));
            match make_entry(dir, name) {
                Ok(entry) => {
                    let meta = entry.meta.clone();
                    if known.insert(id) {
                        changes.push(Change::Added(entry));
                    } else if let Some(meta) = meta {
                        changes.push(Change::Updated(id, meta));
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    if known.remove(&id) {
                        changes.push(Change::Removed(id));
                    }
                }
                Err(_) => {}
            }
        }
        changes
    }

    /// The kernel dropped events: compare the name set again and re-stat everything still there.
    fn rescan(&mut self, dir: &Path, known: &mut HashSet<EntryId>, changes: &mut Vec<Change>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        let mut present = HashSet::new();
        for de in rd.flatten() {
            let name = de.file_name();
            present.insert(EntryId::of(&name_string(&name)));
            self.touch(&name);
        }
        let gone: Vec<EntryId> = known.iter().filter(|id| !present.contains(id)).copied().collect();
        for id in gone {
            known.remove(&id);
            changes.push(Change::Removed(id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::os::unix::fs::{PermissionsExt, symlink};

    const WAIT: Duration = Duration::from_secs(10);

    struct Collected {
        entries: Vec<Vec<Entry>>,
        metas: HashMap<EntryId, Meta>,
        meta_chunks: usize,
        order: Vec<&'static str>,
        listed: Option<usize>,
        error: Option<ListError>,
    }

    fn collect_until_complete(rx: &flume::Receiver<ListEvent>) -> Collected {
        let mut c = Collected {
            entries: vec![],
            metas: HashMap::new(),
            meta_chunks: 0,
            order: vec![],
            listed: None,
            error: None,
        };
        loop {
            match rx.recv_timeout(WAIT).expect("event before timeout") {
                ListEvent::Entries(e) => {
                    c.order.push("entries");
                    c.entries.push(e);
                }
                ListEvent::Meta(m) => {
                    c.order.push("meta");
                    c.meta_chunks += 1;
                    for (id, meta) in m {
                        assert!(c.metas.insert(id, meta).is_none(), "meta sent twice");
                    }
                }
                ListEvent::Listed { total } => {
                    c.order.push("listed");
                    c.listed = Some(total);
                }
                ListEvent::Complete => {
                    c.order.push("complete");
                    return c;
                }
                ListEvent::Error(e) => {
                    c.error = Some(e);
                    return c;
                }
                ListEvent::Slow | ListEvent::Changed(_) => {}
            }
        }
    }

    fn touch_many(dir: &Path, n: usize) {
        for i in 0..n {
            fs::write(dir.join(format!("f{i:05}.txt")), b"x").expect("write");
        }
    }

    fn next_changes(rx: &flume::Receiver<ListEvent>, want: impl Fn(&[Change]) -> bool) -> Vec<Change> {
        let mut all = Vec::new();
        let end = Instant::now() + WAIT;
        while Instant::now() < end {
            if let Ok(ListEvent::Changed(c)) = rx.recv_timeout(Duration::from_millis(200)) {
                all.extend(c);
                if want(&all) {
                    return all;
                }
            }
        }
        panic!("expected changes never arrived, got {all:?}");
    }

    #[test]
    fn chunks_and_meta_for_every_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        touch_many(dir.path(), 5000);
        let listing = list(dir.path().to_path_buf(), ListOptions::default());
        let c = collect_until_complete(&listing.events);
        assert_eq!(c.entries[0].len(), FIRST_CHUNK);
        assert!(c.entries.iter().all(|e| e.len() <= MAX_CHUNK));
        let total: usize = c.entries.iter().map(Vec::len).sum();
        assert_eq!((total, c.listed), (5000, Some(5000)));
        assert_eq!(c.metas.len(), 5000);
        let ids: HashSet<_> = c.entries.iter().flatten().map(|e| e.id).collect();
        assert_eq!(ids.len(), 5000);
        assert!(c.metas.keys().all(|id| ids.contains(id)));
        assert!(c.entries.iter().flatten().all(|e| e.meta.is_none()));
        assert!(c.metas.values().all(|m| m.size == 1 && m.kind == EntryKind::File && m.mode & 0o600 == 0o600));
        assert_eq!(c.order.first(), Some(&"entries"));
        assert_eq!(c.order.last(), Some(&"complete"));
        let listed_at = c.order.iter().position(|o| *o == "listed").expect("listed");
        assert!(c.order[..listed_at].iter().all(|o| *o != "complete"));
        assert!(c.order.iter().rposition(|o| *o == "entries") < Some(listed_at));
        assert!(c.meta_chunks > 1);
    }

    #[test]
    fn first_chunk_precedes_every_meta() {
        let dir = tempfile::tempdir().expect("tempdir");
        touch_many(dir.path(), 3000);
        let listing = list(dir.path().to_path_buf(), ListOptions::default());
        let first = listing.events.recv_timeout(WAIT).expect("first");
        assert!(matches!(first, ListEvent::Entries(ref e) if e.len() == FIRST_CHUNK));
    }

    #[test]
    fn empty_dir_lists_and_completes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let listing = list(dir.path().to_path_buf(), ListOptions::default());
        let c = collect_until_complete(&listing.events);
        assert_eq!(c.order, ["listed", "complete"]);
        assert_eq!(c.listed, Some(0));
    }

    #[test]
    fn kinds_symlinks_and_hidden() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path();
        fs::create_dir(p.join("sub")).expect("mkdir");
        fs::write(p.join("sub/inner"), b"1").expect("write");
        fs::write(p.join("a.txt"), b"hello").expect("write");
        fs::write(p.join(".hidden"), b"").expect("write");
        symlink("a.txt", p.join("to_file")).expect("symlink");
        symlink("sub", p.join("to_dir")).expect("symlink");
        symlink("missing", p.join("broken")).expect("symlink");
        let listing = list(p.to_path_buf(), ListOptions { show_hidden: false });
        let c = collect_until_complete(&listing.events);
        let by_name: HashMap<&str, (&Entry, &Meta)> =
            c.entries.iter().flatten().map(|e| (&*e.name, (e, &c.metas[&e.id]))).collect();
        assert_eq!(by_name.len(), 6);
        assert_eq!(by_name["sub"].0.kind, EntryKind::Dir);
        assert_eq!(by_name["sub"].1.child_count, None);
        assert_eq!(by_name["a.txt"].1.size, 5);
        assert!(by_name[".hidden"].0.hidden);
        assert_eq!(by_name["to_file"].0.kind, EntryKind::Symlink { to_dir: false, broken: false });
        assert_eq!(by_name["to_file"].1.kind, EntryKind::Symlink { to_dir: false, broken: false });
        assert_eq!(by_name["to_dir"].1.kind, EntryKind::Symlink { to_dir: true, broken: false });
        assert_eq!(by_name["broken"].1.kind, EntryKind::Symlink { to_dir: false, broken: true });
        assert!(by_name["to_dir"].1.kind.is_dir() && !by_name["broken"].1.kind.is_dir());
        assert!(by_name["a.txt"].1.modified.is_some());
    }

    #[test]
    fn errors_map() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = |p: PathBuf| collect_until_complete(&list(p, ListOptions::default()).events).error;
        assert_eq!(err(dir.path().join("nope")), Some(ListError::NotFound));
        fs::write(dir.path().join("file"), b"").expect("write");
        assert_eq!(err(dir.path().join("file")), Some(ListError::NotADirectory));
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } != 0 {
            let locked = dir.path().join("locked");
            fs::create_dir(&locked).expect("mkdir");
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("chmod");
            let got = err(locked.clone());
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).expect("chmod back");
            assert_eq!(got, Some(ListError::PermissionDenied));
        }
    }

    #[test]
    fn drop_cancels_promptly() {
        let dir = tempfile::tempdir().expect("tempdir");
        touch_many(dir.path(), 20_000);
        let listing = list(dir.path().to_path_buf(), ListOptions::default());
        let rx = listing.events.clone();
        assert!(matches!(rx.recv_timeout(WAIT), Ok(ListEvent::Entries(_))));
        let started = Instant::now();
        drop(listing);
        // Every thread holding a sender has exited once the channel disconnects.
        loop {
            match rx.recv_timeout(Duration::from_secs(2)) {
                Err(flume::RecvTimeoutError::Disconnected) => break,
                Err(flume::RecvTimeoutError::Timeout) => panic!("threads still running 2 s after drop"),
                Ok(_) => {}
            }
        }
        assert!(started.elapsed() < Duration::from_secs(1), "took {:?}", started.elapsed());
    }

    #[test]
    fn drop_ends_the_watcher() {
        let dir = tempfile::tempdir().expect("tempdir");
        let listing = list(dir.path().to_path_buf(), ListOptions::default());
        let rx = listing.events.clone();
        collect_until_complete(&rx);
        let started = Instant::now();
        drop(listing);
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)), Err(flume::RecvTimeoutError::Disconnected));
        assert!(started.elapsed() < Duration::from_millis(500), "took {:?}", started.elapsed());
    }

    #[test]
    fn watcher_reports_add_update_remove() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("keep"), b"1").expect("write");
        let listing = list(dir.path().to_path_buf(), ListOptions::default());
        collect_until_complete(&listing.events);
        let id = EntryId::of("new.txt");

        fs::write(dir.path().join("new.txt"), b"abc").expect("write");
        let c = next_changes(&listing.events, |c| c.iter().any(|c| matches!(c, Change::Added(_))));
        let added = c.iter().find_map(|c| if let Change::Added(e) = c { Some(e) } else { None }).expect("added");
        assert_eq!((&*added.name, added.id), ("new.txt", id));
        assert_eq!(added.meta.as_ref().map(|m| m.size), Some(3));

        fs::write(dir.path().join("new.txt"), b"abcdef").expect("write");
        let c = next_changes(&listing.events, |c| c.iter().any(|c| matches!(c, Change::Updated(..))));
        assert!(c.iter().any(|c| matches!(c, Change::Updated(i, m) if *i == id && m.size == 6)));

        fs::remove_file(dir.path().join("new.txt")).expect("rm");
        let c = next_changes(&listing.events, |c| !c.is_empty());
        assert_eq!(c, [Change::Removed(id)]);
    }

    #[test]
    fn watcher_coalesces_create_then_delete() {
        let dir = tempfile::tempdir().expect("tempdir");
        let listing = list(dir.path().to_path_buf(), ListOptions::default());
        collect_until_complete(&listing.events);
        fs::write(dir.path().join("blink"), b"").expect("write");
        fs::remove_file(dir.path().join("blink")).expect("rm");
        fs::write(dir.path().join("stay"), b"").expect("write");
        let c = next_changes(&listing.events, |c| !c.is_empty());
        assert!(matches!(c.as_slice(), [Change::Added(e)] if &*e.name == "stay"), "{c:?}");
    }

    #[test]
    fn watcher_pairs_renames() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("old.txt"), b"data").expect("write");
        let listing = list(dir.path().to_path_buf(), ListOptions::default());
        collect_until_complete(&listing.events);

        fs::rename(dir.path().join("old.txt"), dir.path().join("renamed.txt")).expect("rename");
        let c = next_changes(&listing.events, |c| !c.is_empty());
        assert!(
            matches!(c.as_slice(), [Change::Renamed { from, to }] if *from == EntryId::of("old.txt") && &*to.name == "renamed.txt"),
            "{c:?}"
        );

        let outside = tempfile::tempdir().expect("tempdir");
        fs::rename(dir.path().join("renamed.txt"), outside.path().join("moved")).expect("move out");
        let c = next_changes(&listing.events, |c| !c.is_empty());
        assert_eq!(c, [Change::Removed(EntryId::of("renamed.txt"))]);

        fs::rename(outside.path().join("moved"), dir.path().join("back")).expect("move in");
        let c = next_changes(&listing.events, |c| !c.is_empty());
        assert!(matches!(c.as_slice(), [Change::Added(e)] if &*e.name == "back"), "{c:?}");
    }

    #[test]
    fn watcher_sees_changes_made_during_the_listing() {
        let dir = tempfile::tempdir().expect("tempdir");
        touch_many(dir.path(), 1000);
        let listing = list(dir.path().to_path_buf(), ListOptions::default());
        fs::write(dir.path().join("late"), b"").expect("write");
        let mut names: HashSet<String> = HashSet::new();
        let end = Instant::now() + WAIT;
        while !names.contains("late") && Instant::now() < end {
            match listing.events.recv_timeout(Duration::from_millis(200)) {
                Ok(ListEvent::Entries(e)) => names.extend(e.iter().map(|e| e.name.to_string())),
                Ok(ListEvent::Changed(c)) => {
                    names.extend(
                        c.iter().filter_map(|c| if let Change::Added(e) = c { Some(e.name.to_string()) } else { None }),
                    );
                }
                _ => {}
            }
        }
        assert!(names.contains("late"));
    }

    #[test]
    fn watched_dir_removal_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sub = dir.path().join("sub");
        fs::create_dir(&sub).expect("mkdir");
        let listing = list(sub.clone(), ListOptions::default());
        collect_until_complete(&listing.events);
        fs::remove_dir(&sub).expect("rmdir");
        let end = Instant::now() + WAIT;
        loop {
            assert!(Instant::now() < end, "no error after the directory was removed");
            if let Ok(ListEvent::Error(e)) = listing.events.recv_timeout(Duration::from_millis(200)) {
                assert_eq!(e, ListError::NotFound);
                break;
            }
        }
    }

    #[test]
    #[ignore = "benchmark: run with --release -- --ignored --nocapture"]
    fn bench_100k() {
        let dir = tempfile::tempdir().expect("tempdir");
        touch_many(dir.path(), 100_000);
        for round in ["first run", "second run"] {
            let started = Instant::now();
            let listing = list(dir.path().to_path_buf(), ListOptions::default());
            let (mut first, mut listed, mut entries) = (None, None, 0);
            loop {
                match listing.events.recv_timeout(Duration::from_secs(60)).expect("event") {
                    ListEvent::Entries(e) => {
                        first.get_or_insert(started.elapsed());
                        entries += e.len();
                    }
                    ListEvent::Listed { .. } => listed = Some(started.elapsed()),
                    ListEvent::Complete => break,
                    _ => {}
                }
            }
            println!(
                "{round}: first Entries {:?}, Listed {:?}, Complete {:?}, {entries} entries",
                first.expect("first"),
                listed.expect("listed"),
                started.elapsed()
            );
        }
    }
}
