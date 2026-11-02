//! The app's one search index. It loads nothing at startup: the first frame draws, then about
//! two seconds later the index starts loading (or building) at idle priority on a thread of
//! its own.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use nd::prelude::*;
use trove_search::Search;

/// How long after the first frame indexing waits, so startup keeps the disk to itself.
const START_AFTER: Duration = Duration::from_secs(2);

struct Shared(Arc<Search>);

pub fn cache_dir() -> PathBuf {
    match crate::thumbs::cache_root() {
        Some(root) => root.join("search"),
        None => nd::system::app_dirs().cache(),
    }
}

pub fn get(cx: &mut App) -> Arc<Search> {
    if let Some(shared) = cx.try_global::<Shared>() {
        return shared.0.clone();
    }
    let search = Arc::new(Search::new(cache_dir(), crate::env::get().home.clone()));
    cx.set_global(Shared(search.clone()));
    search
}

/// Called on every frame of the window; starts the index once, [`START_AFTER`] past the first.
pub fn after_first_frame(cx: &mut App) {
    static ARMED: AtomicBool = AtomicBool::new(false);
    if !crate::pump::live() || ARMED.swap(true, Ordering::Relaxed) {
        return;
    }
    let search = get(cx);
    let (stop, wait) = flume::bounded::<()>(1);
    let _ = std::thread::Builder::new().name("trove-index-start".into()).spawn(move || {
        if wait.recv_timeout(START_AFTER) == Err(flume::RecvTimeoutError::Timeout) {
            search.start_indexing();
        }
    });
    cx.set_global(Stop { _wake: stop });
}

/// Dropping it (the app quitting) wakes the waiting thread early, which then starts nothing.
struct Stop {
    _wake: flume::Sender<()>,
}
