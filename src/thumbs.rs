//! The app's one preview engine and thumbnail pool. The grid asks for a thumbnail only from a
//! tile being drawn, gets it back on the main thread, and bumps that tile alone.
//!
//! Under the demo fixture every cache sits beside the fixture, never in the user's
//! `~/.cache/thumbnails`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime};

use nd::prelude::*;
use trove_fs::{Entry, EntryId, FileKind, Volume};
use trove_preview::{Config, Engine, ThumbSize, Thumbnails};

use crate::pump::pump;
use crate::tab::Tab;

/// Half the cores, two to four: decoding is CPU bound, and the listing's stat pool and the
/// window want the rest.
fn threads() -> usize {
    std::thread::available_parallelism().map_or(2, |n| (n.get() / 2).clamp(2, 4))
}
/// Arrivals kept for tabs that have not caught up yet.
const RECENT: usize = 4096;

/// Where caches go: the demo's own root beside the fixture, else the user's cache dirs.
pub fn cache_root() -> Option<PathBuf> {
    let env = crate::env::get();
    env.demo.then(|| env.home.with_file_name(format!("{}-cache", file_name(&env.home))))
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "trove".to_owned())
}

/// The engine every preview and thumbnail goes through.
pub fn engine() -> Arc<Engine> {
    static ENGINE: OnceLock<Arc<Engine>> = OnceLock::new();
    ENGINE
        .get_or_init(|| {
            let mut config = Config::from_env();
            if let Some(root) = cache_root() {
                config.thumbnails_dir = root.join("thumbnails");
                config.covers_dir = root.join("covers");
            }
            Engine::new(config)
        })
        .clone()
}

/// Kinds a thumbnailer can draw; the rest keep their icon without asking.
pub fn has_thumbnail(kind: FileKind) -> bool {
    matches!(
        kind,
        FileKind::Image
            | FileKind::Video
            | FileKind::Pdf
            | FileKind::Font
            | FileKind::Document
            | FileKind::Spreadsheet
            | FileKind::Presentation
    )
}

pub struct Thumbs {
    pool: Thumbnails,
    rx: flume::Receiver<(PathBuf, Option<PathBuf>)>,
    done: HashMap<PathBuf, Option<PathBuf>>,
    asked: HashSet<PathBuf>,
    arrivals: VecDeque<(u64, PathBuf)>,
    seq: u64,
    remote: HashMap<PathBuf, bool>,
    volumes: Option<Vec<Volume>>,
}

struct Shared(Entity<Thumbs>);

/// The app's pool, made on first use.
pub fn get(cx: &mut App) -> Entity<Thumbs> {
    if let Some(shared) = cx.try_global::<Shared>() {
        return shared.0.clone();
    }
    let entity = cx.new(|cx| {
        let (pool, rx) = Thumbnails::with_engine(engine(), threads());
        pump(rx.clone(), cx.weak_entity(), |t: &mut Thumbs, batch, cx| {
            t.arrive(batch, cx);
            true
        });
        Thumbs {
            pool,
            rx,
            done: HashMap::new(),
            asked: HashSet::new(),
            arrivals: VecDeque::new(),
            seq: 0,
            remote: HashMap::new(),
            volumes: None,
        }
    });
    cx.set_global(Shared(entity.clone()));
    entity
}

impl Thumbs {
    /// The thumbnail of `path` when it is made; otherwise asks for it once, if this kind can
    /// have one and the folder is not on a network mount.
    pub fn lookup(&mut self, path: &Path, kind: FileKind, mtime: Option<SystemTime>, network: bool) -> Option<PathBuf> {
        if let Some(done) = self.done.get(path) {
            return done.clone();
        }
        if !has_thumbnail(kind) || self.asked.contains(path) || !(network || self.local(path)) {
            return None;
        }
        let mtime = mtime?;
        self.asked.insert(path.to_path_buf());
        self.pool.request(path.to_path_buf(), mtime, ThumbSize::Normal);
        None
    }

    fn local(&mut self, path: &Path) -> bool {
        let Some(dir) = path.parent() else { return false };
        if let Some(remote) = self.remote.get(dir) {
            return !remote;
        }
        let volumes = self.volumes.get_or_insert_with(crate::env::volumes);
        let remote = trove_fs::is_network_path(dir, volumes);
        self.remote.insert(dir.to_path_buf(), remote);
        !remote
    }

    /// Forgets every request not started: the folder they were for is gone.
    pub fn cancel_all(&mut self) {
        self.pool.cancel_all();
        self.asked.clear();
        self.volumes = None;
        self.remote.clear();
    }

    fn arrive(&mut self, batch: Vec<(PathBuf, Option<PathBuf>)>, cx: &mut Context<Self>) {
        for (path, thumb) in batch {
            self.asked.remove(&path);
            self.seq += 1;
            self.arrivals.push_back((self.seq, path.clone()));
            self.done.insert(path, thumb);
        }
        if self.asked.is_empty() {
            crate::trace::mark("thumbnails idle");
        }
        while self.arrivals.len() > RECENT {
            self.arrivals.pop_front();
        }
        cx.notify();
    }

    /// What arrived after `seen`, and the sequence number to pass next time.
    pub fn since(&self, seen: u64) -> (Vec<PathBuf>, u64) {
        let new = self.arrivals.iter().filter(|(n, _)| *n > seen).map(|(_, p)| p.clone()).collect();
        (new, self.seq)
    }

    /// Applies what the pool sent within `timeout`: the host's pump, for tests.
    pub fn drain(&mut self, timeout: Duration, cx: &mut Context<Self>) {
        let mut batch = Vec::new();
        while !self.asked.is_empty() {
            match self.rx.recv_timeout(timeout) {
                Ok(first) => {
                    batch.push(first);
                    batch.extend(self.rx.try_iter());
                    let ready = std::mem::take(&mut batch);
                    self.arrive(ready, cx);
                }
                Err(_) => break,
            }
        }
    }
}

/// The picture of a grid tile: the thumbnail once it exists, the kind's icon until then.
pub fn tile_art(path: Option<PathBuf>, e: &Entry, lit: bool, cx: &mut Context<Tab>) -> AnyElement {
    let mtime = e.meta.as_ref().and_then(|m| m.modified);
    let thumb = path.and_then(|p| {
        let network = crate::settings::get(cx).network_thumbnails;
        let thumbs = get(cx);
        thumbs.update(cx, |t, _| t.lookup(&p, e.file_kind, mtime, network))
    });
    let theme = cx.theme();
    let art = match thumb {
        Some(thumb) => image(ImageSource::path(&thumb))
            .contain()
            .w(px(ART.0))
            .h(px(ART.1))
            .rounded(theme.radius.sm)
            .alt(e.name.to_string())
            .into_any_element(),
        None => icon(crate::icons::for_kind(e.file_kind), 48.)
            .text_color(crate::icons::tint(e.file_kind, theme))
            .into_any_element(),
    };
    let plate = div()
        .p(theme.spacing.xs)
        .rounded(theme.radius.md)
        .when(lit, |d| d.bg(theme.pressed(theme.surface.base)))
        .child(art);
    div().flex_1().w_full().flex().items_center().justify_center().child(plate).into_any_element()
}

/// The box a tile's picture takes inside the tile.
const ART: (f32, f32) = (88., 60.);

/// Bumps the tiles whose thumbnail arrived since the tab last looked.
pub fn arrived(tab: &mut Tab, thumbs: &Entity<Thumbs>, cx: &mut Context<Tab>) {
    let (paths, seq) = thumbs.read(cx).since(tab.extras.thumbs_seen);
    tab.extras.thumbs_seen = seq;
    let ids: Vec<EntryId> = paths.iter().filter_map(|p| tab.id_of_path(p)).collect();
    if !ids.is_empty() {
        tab.bump_ids(&ids, cx);
    }
}

/// The thumbnail made for `path`, if one was.
pub fn thumbnail(cx: &App, path: &Path) -> Option<PathBuf> {
    cx.try_global::<Shared>()?.0.read(cx).done.get(path).cloned().flatten()
}

/// Waits up to `timeout` for every thumbnail asked for: the host's pump, for tests.
pub fn drain(timeout: Duration, cx: &mut App) {
    let Some(shared) = cx.try_global::<Shared>() else { return };
    let thumbs = shared.0.clone();
    thumbs.update(cx, |t, cx| t.drain(timeout, cx));
}

/// Drops queued thumbnail work: a tab moved away from the folder it was for.
pub fn cancel(cx: &mut App) {
    if let Some(shared) = cx.try_global::<Shared>() {
        let thumbs = shared.0.clone();
        thumbs.update(cx, |t, _| t.cancel_all());
    }
}
