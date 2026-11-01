//! Name search: a persisted index of the home directory plus a live parallel walk.

mod index;
mod live;
mod matcher;
mod mounts;
mod overlay;
mod persist;
mod scan;
mod synth;
mod text;
mod time;
mod walk;

use std::collections::HashSet;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed, Ordering::SeqCst};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use index::Index;
use live::LiveOpts;
use matcher::Matcher;
use mounts::{is_slow, mount_of};
use overlay::{Note, Overlay};
use scan::{Ranked, Scan};
use text::fold;

/// An index older than this is rebuilt in the background, the old one serving meanwhile.
const REBUILD_AFTER: Duration = Duration::from_secs(30 * 60);
/// An index older than this can miss files made since, so a query also walks its scope three
/// levels deep, where new files mostly land.
const SHALLOW_AFTER: Duration = Duration::from_secs(5 * 60);
const SHALLOW_DEPTH: usize = 3;
/// A walk of a network scope stops at this depth and this many entries, and says so.
const REMOTE_DEPTH: usize = 4;
const REMOTE_BUDGET: u64 = 20_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IndexState {
    Absent,
    Loading,
    Building { scanned: u64 },
    Ready { entries: u64, age: Duration },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    pub text: String,
    pub scope: PathBuf,
    pub show_hidden: bool,
    pub limit: usize,
}

pub struct SearchRun {
    pub events: flume::Receiver<SearchEvent>,
    _cancel: CancelOnDrop,
}

struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Relaxed);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchEvent {
    /// The best hits so far, best first; replaces the previous set.
    Hits(Vec<Hit>),
    /// `truncated`: a network scope's walk stopped at its depth or entry cap.
    Done { scanned: u64, elapsed_ms: u64, truncated: bool },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub path: PathBuf,
    pub is_dir: bool,
    pub score: i32,
    /// Byte ranges `[start, end)` of the file name (the path's last component) that matched.
    pub name_matches: Vec<(u32, u32)>,
}

/// One per app. Every method returns at once; loading, building and querying run on threads
/// of their own.
pub struct Search {
    inner: Arc<Inner>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Absent,
    Loading,
    Building,
    Ready,
}

struct Inner {
    cache_dir: PathBuf,
    root: PathBuf,
    index: RwLock<Option<Arc<Index>>>,
    phase: Mutex<Phase>,
    scanned: AtomicU64,
    started: AtomicBool,
    building: AtomicBool,
    shutdown: AtomicBool,
    generation: AtomicU64,
    pending: Mutex<Vec<Note>>,
    overlay: Mutex<Arc<Overlay>>,
    /// Held while notes are applied (they stat and may walk), never by the caller's thread.
    notes_lock: Mutex<()>,
}

impl Search {
    pub fn new(cache_dir: PathBuf, home: PathBuf) -> Search {
        Search {
            inner: Arc::new(Inner {
                cache_dir,
                root: home,
                index: RwLock::new(None),
                phase: Mutex::new(Phase::Absent),
                scanned: AtomicU64::new(0),
                started: AtomicBool::new(false),
                building: AtomicBool::new(false),
                shutdown: AtomicBool::new(false),
                generation: AtomicU64::new(0),
                pending: Mutex::new(Vec::new()),
                overlay: Mutex::new(Arc::new(Overlay::default())),
                notes_lock: Mutex::new(()),
            }),
        }
    }

    pub fn indexed_root(&self) -> &Path {
        &self.inner.root
    }

    pub fn start_indexing(&self) {
        if self.inner.started.swap(true, SeqCst) {
            return;
        }
        let inner = self.inner.clone();
        let _ = std::thread::Builder::new().name("trove-index".into()).spawn(move || {
            inner.set_phase(Phase::Loading);
            match persist::load(&inner.index_path(), &inner.root) {
                Ok(ix) => {
                    let stale = ix.age() >= REBUILD_AFTER;
                    inner.install(ix);
                    if stale {
                        inner.rebuild();
                    }
                }
                Err(_) => inner.rebuild(),
            }
        });
    }

    pub fn index_state(&self) -> IndexState {
        let phase = *lock(&self.inner.phase);
        match phase {
            Phase::Absent => IndexState::Absent,
            Phase::Loading => IndexState::Loading,
            Phase::Building => IndexState::Building { scanned: self.inner.scanned.load(Relaxed) },
            Phase::Ready => match self.inner.current() {
                Some(ix) => IndexState::Ready { entries: ix.len() as u64 - 1, age: ix.age() },
                None => IndexState::Absent,
            },
        }
    }

    pub fn query(&self, q: Query) -> SearchRun {
        let (tx, rx) = flume::unbounded();
        let cancel = Arc::new(AtomicBool::new(false));
        let inner = self.inner.clone();
        let flag = cancel.clone();
        let _ = std::thread::Builder::new().name("trove-query".into()).spawn(move || run_query(&inner, q, &tx, &flag));
        SearchRun { events: rx, _cancel: CancelOnDrop(cancel) }
    }

    /// The UI made `path` (a new file or folder, a copy, an extraction). Queries see it at once.
    pub fn note_created(&self, path: PathBuf) {
        lock(&self.inner.pending).push(Note::Created(path));
    }

    /// The UI removed `path` and everything under it.
    pub fn note_removed(&self, path: PathBuf) {
        lock(&self.inner.pending).push(Note::Removed(path));
    }

    /// The UI renamed or moved `from` to `to`, a folder's contents with it.
    pub fn note_renamed(&self, from: PathBuf, to: PathBuf) {
        lock(&self.inner.pending).push(Note::Renamed(from, to));
    }
}

impl Drop for Search {
    fn drop(&mut self) {
        self.inner.shutdown.store(true, Relaxed);
    }
}

impl Inner {
    fn index_path(&self) -> PathBuf {
        self.cache_dir.join(persist::file_name())
    }

    fn current(&self) -> Option<Arc<Index>> {
        self.index.read().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn set_phase(&self, p: Phase) {
        *lock(&self.phase) = p;
    }

    fn install(self: &Arc<Self>, mut ix: Index) {
        ix.generation = self.generation.fetch_add(1, SeqCst) + 1;
        *self.index.write().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(ix));
        self.set_phase(Phase::Ready);
        self.snapshot();
    }

    fn rebuild(self: &Arc<Self>) {
        if self.building.swap(true, SeqCst) {
            return;
        }
        walk::lower_priority();
        let had = self.current().is_some();
        if !had {
            self.set_phase(Phase::Building);
        }
        self.scanned.store(0, Relaxed);
        match walk::build_index(&self.root, &self.scanned, &self.shutdown) {
            Ok(ix) => {
                let _ = persist::save(&ix, &self.index_path());
                self.install(ix);
            }
            Err(_) if !had => self.set_phase(Phase::Absent),
            Err(_) => {}
        }
        self.building.store(false, SeqCst);
    }

    fn request_rebuild(self: &Arc<Self>) {
        if !self.started.load(SeqCst) || self.building.load(SeqCst) {
            return;
        }
        let inner = self.clone();
        let _ = std::thread::Builder::new().name("trove-index".into()).spawn(move || inner.rebuild());
    }

    /// The current index with an overlay that matches it and holds every note made so far.
    fn snapshot(self: &Arc<Self>) -> (Option<Arc<Index>>, Arc<Overlay>) {
        let _held = lock(&self.notes_lock);
        let ix = self.current();
        let notes = std::mem::take(&mut *lock(&self.pending));
        let cur = lock(&self.overlay).clone();
        let generation = ix.as_ref().map_or(0, |i| i.generation);
        if notes.is_empty() && cur.generation == generation {
            return (ix, cur);
        }
        let mut ov = (*cur).clone();
        if ov.generation != generation {
            ov.rebase(ix.as_deref());
        }
        let mut rebuild = false;
        for n in notes {
            rebuild |= ov.apply(n, ix.as_deref());
        }
        let ov = Arc::new(ov);
        *lock(&self.overlay) = ov.clone();
        if rebuild {
            self.request_rebuild();
        }
        (ix, ov)
    }
}

fn run_query(inner: &Arc<Inner>, q: Query, tx: &flume::Sender<SearchEvent>, cancel: &AtomicBool) {
    let t0 = Instant::now();
    let finish = |scanned: u64, truncated: bool| {
        let elapsed_ms = t0.elapsed().as_millis() as u64;
        let _ = tx.send(SearchEvent::Done { scanned, elapsed_ms, truncated });
    };
    let m = Matcher::parse(&q.text, q.show_hidden, now_secs() as i64);
    if q.limit == 0 || m.is_empty() {
        let _ = tx.send(SearchEvent::Hits(Vec::new()));
        finish(0, false);
        return;
    }
    let (ix, ov) = inner.snapshot();
    let mounts = mounts::read_mounts();
    let slow = mount_of(&q.scope, &mounts).is_some_and(is_slow);
    let mut last: Option<Vec<(u64, PathBuf)>> = None;
    let mut send = |list: &[Ranked]| {
        let now: Vec<(u64, PathBuf)> = list.iter().map(|r| (r.key, r.path.clone())).collect();
        if last.as_ref() == Some(&now) {
            return true;
        }
        last = Some(now);
        tx.send(SearchEvent::Hits(to_hits(&m, list))).is_ok()
    };

    let covered = ix
        .as_ref()
        .filter(|_| !slow)
        .and_then(|ix| ix.lookup(&q.scope).filter(|&s| ix.is_dir(s) && !ov.is_tombstoned(s)).map(|s| (ix, s)));
    if let Some((ix, s)) = covered {
        let ranked = index_query(ix, &ov, &m, s, &q, cancel);
        if cancel.load(Relaxed) {
            return;
        }
        send(&ranked);
        let mut scanned = u64::from(ix.end[s as usize] - s - 1);
        if ix.age() >= SHALLOW_AFTER {
            let opts = LiveOpts { max_depth: Some(SHALLOW_DEPTH), budget: None, report_cut: false, limit: q.limit };
            scanned += live::search(&q.scope, &m, opts, &ranked, &mounts, cancel, |l| send(&l)).scanned;
        }
        if ix.age() >= REBUILD_AFTER {
            inner.request_rebuild();
        }
        if !cancel.load(Relaxed) {
            finish(scanned, false);
        }
        return;
    }

    let opts = if slow {
        LiveOpts { max_depth: Some(REMOTE_DEPTH), budget: Some(REMOTE_BUDGET), report_cut: true, limit: q.limit }
    } else {
        LiveOpts { max_depth: None, budget: None, report_cut: false, limit: q.limit }
    };
    let r = live::search(&q.scope, &m, opts, &[], &mounts, cancel, |l| send(&l));
    if cancel.load(Relaxed) {
        return;
    }
    if last.is_none() {
        let _ = tx.send(SearchEvent::Hits(Vec::new()));
    }
    finish(r.scanned, r.truncated);
}

fn index_query(ix: &Index, ov: &Overlay, m: &Matcher, s: u32, q: &Query, cancel: &AtomicBool) -> Vec<Ranked> {
    let scan = Scan::new(ix, m, s, &ov.tomb, cancel);
    let within = m.within_left(&fold(q.scope.file_name().map_or(&b""[..], |n| n.as_bytes())));
    let ranked = |cands: Vec<scan::Cand>, fuzzy: bool| {
        cands.into_iter().map(move |c| Ranked { key: c.key, path: ix.path_of(c.idx), is_dir: ix.is_dir(c.idx), fuzzy })
    };
    let (c, n) = scan.run(false, q.limit);
    let (extra, xn) = ov.matches(m, &q.scope, &within, false);
    let mut out: Vec<Ranked> = ranked(c, false).chain(extra).collect();
    if ((n + xn) as usize) < q.limit && m.has_fuzzy() && !cancel.load(Relaxed) {
        let (c, _) = scan.run(true, q.limit);
        out.extend(ranked(c, true));
        out.extend(ov.matches(m, &q.scope, &within, true).0);
    }
    out.sort_unstable_by(|a, b| b.cmp(a));
    let mut seen = HashSet::with_capacity(out.len());
    out.retain(|r| seen.insert(r.path.clone()));
    out.truncate(q.limit);
    out
}

fn to_hits(m: &Matcher, list: &[Ranked]) -> Vec<Hit> {
    list.iter()
        .map(|r| {
            let name = r.path.file_name().map_or(&b""[..], |n| n.as_bytes());
            Hit {
                path: r.path.clone(),
                is_dir: r.is_dir,
                score: (r.key >> 32) as i32,
                name_matches: m.highlights(name, &fold(name), r.fuzzy),
            }
        })
        .collect()
}

pub(crate) fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Writes an index of `entries` generated names under `root` (which need not exist) into
/// `cache_dir`, for measuring queries at sizes no real home has.
#[doc(hidden)]
pub fn write_synthetic_index(cache_dir: &Path, root: &Path, entries: usize) -> io::Result<()> {
    let ix = Index::from_raw(root.to_path_buf(), synth::generate(root, entries), now_secs());
    persist::save(&ix, &cache_dir.join(persist::file_name()))
}

#[cfg(test)]
mod tests;
