use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use notify::event::{EventKind, ModifyKind, RenameMode};
use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};

use super::{Batch, COALESCE, Ctx, EntryId, ListError, ListEvent, MAX_CHUNK};

pub(super) enum Msg {
    Fs(notify::Event),
    Wake,
}

/// The watcher's events and the cancel wake share one channel, so a dropped listing ends the
/// watch without polling.
pub(super) struct Cancel {
    flag: AtomicBool,
    tx: flume::Sender<Msg>,
    rx: flume::Receiver<Msg>,
}

impl Cancel {
    pub(super) fn new() -> Self {
        let (tx, rx) = flume::unbounded();
        Self { flag: AtomicBool::new(false), tx, rx }
    }

    pub(super) fn is_set(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }

    pub(super) fn trigger(&self) {
        self.flag.store(true, Ordering::Relaxed);
        let _ = self.tx.send(Msg::Wake);
    }
}

pub(super) struct Watcher {
    _watcher: RecommendedWatcher,
    /// FSEvents reports resolved paths: `/private/var/..` for a folder opened as `/var/..`.
    canonical: PathBuf,
}

impl Watcher {
    pub(super) fn new(dir: &Path, cancel: &Cancel) -> Option<Self> {
        let tx = cancel.tx.clone();
        let mut watcher = notify::recommended_watcher(move |ev: notify::Result<notify::Event>| {
            if let Ok(ev) = ev {
                let _ = tx.send(Msg::Fs(ev));
            }
        })
        .ok()?;
        watcher.watch(dir, RecursiveMode::NonRecursive).ok()?;
        let canonical = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        Some(Self { _watcher: watcher, canonical })
    }

    pub(super) fn run(self, dir: &Path, mut known: HashSet<EntryId>, ctx: &Ctx) {
        let rx = &ctx.cancel.rx;
        let mut batch = Batch::default();
        let mut pending_from: Option<OsString> = None;
        let mut deadline: Option<Instant> = None;
        loop {
            let msg = match deadline {
                Some(d) => match rx.recv_deadline(d) {
                    Ok(m) => Some(m),
                    Err(flume::RecvTimeoutError::Timeout) => None,
                    Err(flume::RecvTimeoutError::Disconnected) => return,
                },
                None => match rx.recv() {
                    Ok(m) => Some(m),
                    Err(_) => return,
                },
            };
            if ctx.cancel.is_set() {
                return;
            }
            if let Some(Msg::Fs(ev)) = msg {
                self.add(&mut batch, &mut pending_from, ev, dir);
                deadline.get_or_insert_with(|| Instant::now() + COALESCE);
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                deadline = None;
                pending_from = None;
                let changes = std::mem::take(&mut batch).resolve(dir, &mut known);
                for part in changes.chunks(MAX_CHUNK) {
                    if !ctx.send(ListEvent::Changed(part.to_vec())) {
                        return;
                    }
                }
                if matches!(fs::symlink_metadata(dir), Err(e) if e.kind() == io::ErrorKind::NotFound) {
                    ctx.send(ListEvent::Error(ListError::NotFound));
                    return;
                }
            }
        }
    }

    /// FSEvents reports each side of a rename as its own unpaired event and ReadDirectoryChangesW
    /// sends From then To, so consecutive rename names pair up; `resolve` keeps a pair only when
    /// the old name is gone and the new one exists.
    fn add(&self, batch: &mut Batch, pending_from: &mut Option<OsString>, ev: notify::Event, dir: &Path) {
        if ev.need_rescan() {
            batch.overflow = true;
        }
        for (i, path) in ev.paths.iter().enumerate() {
            let parent = path.parent();
            if parent != Some(self.canonical.as_path()) && parent != Some(dir) {
                continue;
            }
            let Some(name) = path.file_name() else { continue };
            batch.touch(name);
            let EventKind::Modify(ModifyKind::Name(mode)) = ev.kind else { continue };
            let from = match mode {
                RenameMode::From => true,
                RenameMode::To => false,
                RenameMode::Both => i == 0,
                RenameMode::Any | RenameMode::Other => pending_from.is_none(),
            };
            if from {
                *pending_from = Some(name.to_os_string());
            } else if let Some(old) = pending_from.take() {
                batch.renames.push((old, name.to_os_string()));
            }
        }
    }
}
