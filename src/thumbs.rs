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
use trove_preview::{Config, Engine, Made, ThumbSize, Thumbnails};

use crate::pump::pump;
use crate::tab::Tab;
use crate::zoom::{RowSize, TileSize};

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
    rx: flume::Receiver<Made>,
    done: HashMap<(PathBuf, ThumbSize), Option<PathBuf>>,
    asked: HashSet<(PathBuf, ThumbSize)>,
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
    /// The thumbnail of `path` at `size` when it is made; otherwise asks for it once, if this
    /// kind can have one and the folder is not on a network mount. A large one not made yet,
    /// or that could not be made, gives way to a normal one already made.
    pub fn lookup(
        &mut self,
        path: &Path,
        kind: FileKind,
        mtime: Option<SystemTime>,
        network: bool,
        size: ThumbSize,
    ) -> Option<PathBuf> {
        let key = (path.to_path_buf(), size);
        let smaller = match size {
            ThumbSize::Large => self.done.get(&(key.0.clone(), ThumbSize::Normal)).cloned().flatten(),
            ThumbSize::Normal => None,
        };
        if let Some(done) = self.done.get(&key) {
            return done.clone().or(smaller);
        }
        if !has_thumbnail(kind) || self.asked.contains(&key) || !(network || self.local(path)) {
            return smaller;
        }
        let Some(mtime) = mtime else { return smaller };
        self.asked.insert(key);
        self.pool.request(path.to_path_buf(), mtime, size);
        smaller
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

    fn arrive(&mut self, batch: Vec<Made>, cx: &mut Context<Self>) {
        for (path, size, thumb) in batch {
            let key = (path, size);
            self.asked.remove(&key);
            self.seq += 1;
            self.arrivals.push_back((self.seq, key.0.clone()));
            self.done.insert(key, thumb);
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

/// The thumbnail of a listed file once it is made, asked for the first time a row or tile
/// showing it at a picture `long_side` points across is drawn.
pub fn for_entry(path: Option<PathBuf>, e: &Entry, long_side: f32, cx: &mut App) -> Option<PathBuf> {
    let mtime = e.meta.as_ref().and_then(|m| m.modified);
    let size = crate::zoom::thumb_size(long_side);
    path.and_then(|p| {
        let network = crate::settings::get(cx).network_thumbnails;
        let thumbs = get(cx);
        thumbs.update(cx, |t, _| t.lookup(&p, e.file_kind, mtime, network, size))
    })
}

/// The picture of a grid tile: the thumbnail once it exists, the kind's icon until then.
pub fn tile_art(path: Option<PathBuf>, e: &Entry, lit: bool, size: TileSize, cx: &mut Context<Tab>) -> AnyElement {
    let thumb = for_entry(path, e, size.art.0.max(size.art.1), cx);
    let theme = cx.theme();
    let art = match thumb {
        Some(thumb) => image(ImageSource::path(&thumb))
            .contain()
            .w(px(size.art.0))
            .h(px(size.art.1))
            .rounded(theme.radius.sm)
            .alt(e.name.to_string())
            .into_any_element(),
        None => icon(crate::icons::for_kind(e.file_kind), size.glyph)
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

/// The picture at the head of a list row: the thumbnail in the glyph's box once it exists.
pub fn row_art(thumb: Option<PathBuf>, glyph: Icon, tint: Hsla, size: RowSize, theme: &Theme) -> AnyElement {
    let art = match thumb {
        Some(thumb) => {
            image(ImageSource::path(&thumb)).contain().size(px(size.art)).rounded(theme.radius.sm).into_any_element()
        }
        None => icon(glyph, size.glyph).text_color(tint).into_any_element(),
    };
    div().size(px(size.art)).flex_none().flex().items_center().justify_center().child(art).into_any_element()
}

/// Bumps the tiles whose thumbnail arrived since the tab last looked.
pub fn arrived(tab: &mut Tab, thumbs: &Entity<Thumbs>, cx: &mut Context<Tab>) {
    let (paths, seq) = thumbs.read(cx).since(tab.extras.thumbs_seen);
    tab.extras.thumbs_seen = seq;
    tab.extras.columns.thumbnails_arrived(&paths, cx);
    let ids: Vec<EntryId> = paths.iter().filter_map(|p| tab.id_of_path(p)).collect();
    if !ids.is_empty() {
        tab.bump_ids(&ids, cx);
    }
}

/// The thumbnail made for `path`, normal size first, if one was.
pub fn thumbnail(cx: &App, path: &Path) -> Option<PathBuf> {
    let done = &cx.try_global::<Shared>()?.0.read(cx).done;
    [ThumbSize::Normal, ThumbSize::Large]
        .into_iter()
        .find_map(|size| done.get(&(path.to_path_buf(), size)).cloned().flatten())
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
