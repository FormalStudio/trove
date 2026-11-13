//! The icons the OS itself shows for files and folders, where it draws them
//! (`Feature::FileIcons`): an app's own icon, a folder's, the document icon of a file's type.
//! A row asks for the icon it shows; one worker thread asks the OS and the answer comes back to
//! the main thread, where every tab redraws. Until then, and where the OS draws none, a row
//! keeps the glyph of its kind.
//!
//! Plain files share one icon per extension. Folders, apps and files with no extension are
//! asked for by path, since each can carry its own. Equal pictures share their bytes.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use nd::platform::Feature;
use nd::prelude::*;
use trove_fs::FileKind;

use crate::pump::pump;

/// The pixel sizes icons are drawn at; a row takes the first that covers it on a 2x display.
const SIDES: [u32; 5] = [32, 64, 128, 256, 512];

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Of {
    Path(PathBuf),
    Extension(String),
}

type Key = (Of, u32);
type Drawn = (Key, Option<Vec<u8>>);

pub struct SysIcons {
    ask: flume::Sender<(Key, PathBuf)>,
    done: HashMap<Key, Option<ImageSource>>,
    asked: HashSet<Key>,
    /// Pictures by the hash of their bytes: a thousand plain folders hold one.
    pictures: HashMap<u64, ImageSource>,
}

struct Shared(Entity<SysIcons>);

/// The app's icons, made on first use; `None` where the OS draws none or no host pumps.
pub fn get(cx: &mut App) -> Option<Entity<SysIcons>> {
    if !nd::platform::has(Feature::FileIcons) || !crate::pump::live() {
        return None;
    }
    if let Some(shared) = cx.try_global::<Shared>() {
        return Some(shared.0.clone());
    }
    let entity = cx.new(|cx| {
        let (ask, asks) = flume::unbounded::<(Key, PathBuf)>();
        let (tx, rx) = flume::unbounded::<Drawn>();
        let system = nd::system::system();
        let spawned = std::thread::Builder::new().name("trove-icons".into()).spawn(move || {
            for (key, path) in asks.iter() {
                let png = system.shell().file_icon(&path, key.1).ok();
                if tx.send((key, png)).is_err() {
                    break;
                }
            }
        });
        if let Err(e) = spawned {
            eprintln!("trove: no thread for file icons: {e}");
        }
        pump(rx, cx.weak_entity(), |icons: &mut SysIcons, batch, cx| {
            icons.arrive(batch, cx);
            true
        });
        SysIcons { ask, done: HashMap::new(), asked: HashSet::new(), pictures: HashMap::new() }
    });
    cx.set_global(Shared(entity.clone()));
    Some(entity)
}

impl SysIcons {
    fn lookup(&mut self, path: &Path, is_dir: bool, kind: FileKind, points: f32) -> Option<ImageSource> {
        let side = SIDES.into_iter().find(|s| *s as f32 >= points * 2.).unwrap_or(SIDES[SIDES.len() - 1]);
        let extension = path.extension().and_then(|e| e.to_str()).filter(|_| !is_dir && kind != FileKind::Executable);
        let of = match extension {
            Some(e) => Of::Extension(e.to_ascii_lowercase()),
            None => Of::Path(path.to_path_buf()),
        };
        let key = (of, side);
        if let Some(done) = self.done.get(&key) {
            return done.clone();
        }
        if self.asked.insert(key.clone()) {
            let _ = self.ask.send((key, path.to_path_buf()));
        }
        None
    }

    fn arrive(&mut self, batch: Vec<Drawn>, cx: &mut Context<Self>) {
        for (key, png) in batch {
            self.asked.remove(&key);
            let picture = png.map(|png| {
                let mut hasher = DefaultHasher::new();
                png.hash(&mut hasher);
                let hash = hasher.finish();
                let source = || ImageSource::bytes(format!("sysicon-{hash:016x}"), png);
                self.pictures.entry(hash).or_insert_with(source).clone()
            });
            self.done.insert(key, picture);
        }
        cx.notify();
    }
}

/// The OS's icon for the file at `path`, for a box `points` across, once it is drawn; asks for
/// it the first time.
pub fn icon(path: &Path, is_dir: bool, kind: FileKind, points: f32, cx: &mut App) -> Option<ImageSource> {
    let icons = get(cx)?;
    icons.update(cx, |icons, _| icons.lookup(path, is_dir, kind, points))
}
