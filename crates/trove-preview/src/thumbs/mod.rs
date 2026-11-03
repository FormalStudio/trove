//! The freedesktop thumbnail cache: https://specifications.freedesktop.org/thumbnail-spec/latest/

mod imagegen;
mod pngmeta;
pub(crate) mod thumbnailer;
mod uri;

use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::engine::Engine;
use crate::mime;
use pngmeta::ThumbMeta;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ThumbSize {
    /// 128 px on the long side.
    Normal,
    /// 256 px on the long side.
    Large,
}

impl ThumbSize {
    pub fn pixels(self) -> u32 {
        match self {
            Self::Normal => 128,
            Self::Large => 256,
        }
    }

    fn dir(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Large => "large",
        }
    }
}

/// Where the thumbnail of `path` lives, whether or not it exists yet.
pub fn cache_path(engine: &Engine, path: &Path, size: ThumbSize) -> PathBuf {
    engine.config.thumbnails_dir.join(size.dir()).join(format!("{}.png", uri::key_of(&uri::file_uri(path))))
}

fn fail_path(engine: &Engine, key: &str) -> PathBuf {
    engine
        .config
        .thumbnails_dir
        .join("fail")
        .join(concat!("trove-", env!("CARGO_PKG_VERSION")))
        .join(format!("{key}.png"))
}

fn secs(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
        Err(e) => -i64::try_from(e.duration().as_secs()).unwrap_or(i64::MAX),
    }
}

/// A cached PNG is current when its `Thumb::MTime` is the source's mtime in whole seconds and
/// its `Thumb::Size`, when present, is the source's size. The size needs a stat; without it, none.
fn is_current(png: &Path, mtime: i64, source: &Path) -> bool {
    let Some(meta) = pngmeta::read_meta(png) else { return false };
    if meta.mtime != Some(mtime) {
        return false;
    }
    match meta.size {
        Some(size) => fs::metadata(source).is_ok_and(|m| m.len() == size),
        None => true,
    }
}

pub(crate) fn thumbnail(engine: &Engine, path: &Path, mtime: SystemTime, size: ThumbSize) -> Option<PathBuf> {
    let cfg = &engine.config;
    let abs = std::path::absolute(path).ok()?;
    if abs.starts_with(&cfg.thumbnails_dir) {
        return None;
    }
    let uri = uri::file_uri(&abs);
    let key = uri::key_of(&uri);
    let target = cfg.thumbnails_dir.join(size.dir()).join(format!("{key}.png"));
    let mtime_s = secs(mtime);
    if is_current(&target, mtime_s, &abs) {
        return Some(target);
    }
    let fail = fail_path(engine, &key);
    if pngmeta::read_meta(&fail).is_some_and(|m| m.mtime == Some(mtime_s)) {
        return None;
    }

    // The listing's mtime picked the cache entry; what gets recorded is what the file says now.
    let stat = fs::metadata(&abs).ok().filter(|m| m.is_file())?;
    let meta = ThumbMeta::new(&uri, stat.modified().map_or(mtime_s, secs), stat.len());
    let ext = trove_fs::entry::extension_of(abs.file_name()?.to_str()?).to_ascii_lowercase();

    let made = if imagegen::decodable(&ext) {
        if stat.len() > cfg.max_image_bytes {
            return None;
        }
        imagegen::render(&abs, size.pixels())
            .and_then(|r| pngmeta::encode_rgba(r.width, r.height, &r.rgba, &meta).ok())
            .and_then(|png| store(&target, &png))
    } else {
        None
    };
    if made.is_some() {
        return Some(target);
    }

    let mime = mime::mime_of(&abs, false);
    let candidates = engine.registry().for_mime(&mime);
    if candidates.is_empty() && !imagegen::decodable(&ext) {
        return None;
    }
    if let Some(done) = candidates.iter().find_map(|t| external(engine, t, &abs, &uri, &target, size, &meta)) {
        return Some(done);
    }
    // A thumbnailer ran and failed, or the decoder rejected the file: do not try again until it changes.
    if let Ok(png) = pngmeta::encode_empty(&meta) {
        let _ = store(&fail, &png);
    }
    None
}

fn external(
    engine: &Engine,
    t: &thumbnailer::Thumbnailer,
    input: &Path,
    uri: &str,
    target: &Path,
    size: ThumbSize,
    meta: &ThumbMeta,
) -> Option<PathBuf> {
    let dir = target.parent()?;
    make_dir(dir).ok()?;
    let tmp = temp_name(dir);
    let ok = thumbnailer::run(t, input, uri, &tmp, size.pixels(), engine.config.thumbnailer_timeout);
    let finished = ok.then(|| fs::read(&tmp).ok()).flatten().and_then(|bytes| pngmeta::splice_meta(&bytes, meta));
    let Some(bytes) = finished else {
        let _ = fs::remove_file(&tmp);
        return None;
    };
    let placed = pngmeta::write_private(&tmp, &bytes)
        .and_then(|()| fs::set_permissions(&tmp, std::os::unix::fs::PermissionsExt::from_mode(0o600)))
        .and_then(|()| fs::rename(&tmp, target));
    if placed.is_err() {
        let _ = fs::remove_file(&tmp);
        return None;
    }
    Some(target.to_path_buf())
}

fn make_dir(dir: &Path) -> std::io::Result<()> {
    fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
}

fn temp_name(dir: &Path) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    dir.join(format!(".{}-{}.tmp", std::process::id(), N.fetch_add(1, Ordering::Relaxed)))
}

/// Write `png` to `target` through a temp file in the same directory, so a reader never sees half a file.
fn store(target: &Path, png: &[u8]) -> Option<()> {
    let dir = target.parent()?;
    make_dir(dir).ok()?;
    let tmp = temp_name(dir);
    if pngmeta::write_private(&tmp, png).and_then(|()| fs::rename(&tmp, target)).is_err() {
        let _ = fs::remove_file(&tmp);
        return None;
    }
    Some(())
}

struct Job {
    path: PathBuf,
    mtime: SystemTime,
    size: ThumbSize,
}

#[derive(Default)]
struct State {
    /// Newest last; workers pop from the end.
    stack: Vec<Job>,
    queued: HashSet<(PathBuf, ThumbSize)>,
    running: HashSet<(PathBuf, ThumbSize)>,
    closed: bool,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A thumbnail the pool answered: the file, the size asked for, and where the thumbnail is, or
/// `None` when none could be made.
pub type Made = (PathBuf, ThumbSize, Option<PathBuf>);

/// A bounded pool making thumbnails, newest request first: the rows on screen now are the ones
/// asked for last.
pub struct Thumbnails {
    shared: Arc<Shared>,
}

impl Thumbnails {
    /// A pool over the real environment's caches. `threads` is clamped to at least 1.
    pub fn new(threads: usize) -> (Self, flume::Receiver<Made>) {
        Self::with_engine(Engine::global(), threads)
    }

    pub fn with_engine(engine: Arc<Engine>, threads: usize) -> (Self, flume::Receiver<Made>) {
        let (tx, rx) = flume::unbounded();
        let shared = Arc::new(Shared { state: Mutex::new(State::default()), wake: Condvar::new() });
        for i in 0..threads.max(1) {
            let (shared, engine, tx) = (shared.clone(), engine.clone(), tx.clone());
            let spawned = std::thread::Builder::new()
                .name(format!("trove-thumb-{i}"))
                .spawn(move || worker(&shared, &engine, &tx));
            drop(spawned);
        }
        (Self { shared }, rx)
    }

    /// Ask for a thumbnail. A path already queued moves to the front of the line; one being made
    /// right now is left alone and answers once.
    pub fn request(&self, path: PathBuf, mtime: SystemTime, size: ThumbSize) {
        let mut st = self.shared.lock();
        let key = (path.clone(), size);
        if st.running.contains(&key) {
            return;
        }
        if !st.queued.insert(key.clone()) {
            st.stack.retain(|j| !(j.path == path && j.size == size));
        }
        st.stack.push(Job { path, mtime, size });
        drop(st);
        self.shared.wake.notify_one();
    }

    /// Forget everything not started yet. Jobs already running finish and answer.
    pub fn cancel_all(&self) {
        let mut st = self.shared.lock();
        st.stack.clear();
        st.queued.clear();
    }
}

/// Nice 10 for the calling thread, so a burst of decodes on a four-core laptop yields to the
/// window's thread.
fn lower_priority() {
    #[cfg(target_os = "linux")]
    // SAFETY: setpriority on the calling thread's id, with integer arguments.
    unsafe {
        let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
        libc::setpriority(libc::PRIO_PROCESS, tid, 10);
    }
}

impl Drop for Thumbnails {
    fn drop(&mut self) {
        let mut st = self.shared.lock();
        st.closed = true;
        st.stack.clear();
        st.queued.clear();
        drop(st);
        self.shared.wake.notify_all();
    }
}

fn worker(shared: &Shared, engine: &Engine, tx: &flume::Sender<Made>) {
    lower_priority();
    loop {
        let job = {
            let mut st = shared.lock();
            loop {
                if st.closed {
                    return;
                }
                if let Some(job) = st.stack.pop() {
                    let key = (job.path.clone(), job.size);
                    st.queued.remove(&key);
                    st.running.insert(key);
                    break job;
                }
                st = shared.wake.wait(st).unwrap_or_else(|e| e.into_inner());
            }
        };
        let result = thumbnail(engine, &job.path, job.mtime, job.size);
        shared.lock().running.remove(&(job.path.clone(), job.size));
        if tx.send((job.path, job.size, result)).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests;
