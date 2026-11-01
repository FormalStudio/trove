//! Background work reports over flume channels; this brings their messages to the main thread,
//! a batch per hop, into the entity that asked for them.
//!
//! `nd::system::spawn` only finishes on a running host, so the pumps start only once [`live`]
//! is set by `run`. Tier 1 tests have no host: they drain the same channels synchronously
//! (`Tab::drain`), which is why every owner keeps its own clone of the receiver.

use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

use nd::prelude::*;

static LIVE: AtomicBool = AtomicBool::new(false);

/// Called once the app's host runs: from here on channels are pumped.
pub fn set_live() {
    LIVE.store(true, Ordering::Relaxed);
}

pub fn live() -> bool {
    LIVE.load(Ordering::Relaxed)
}

/// The most messages one hop carries, so a flood of stat results never holds a frame long.
const BATCH: usize = 8192;

type Apply<V, T> = Rc<dyn Fn(&mut V, Vec<T>, &mut Context<V>) -> bool>;

/// Delivers `rx`'s messages to `entity` in batches until the senders go away, the entity is
/// released, or `apply` returns false.
pub fn pump<V: 'static, T: Send + 'static>(
    rx: flume::Receiver<T>,
    entity: WeakEntity<V>,
    apply: impl Fn(&mut V, Vec<T>, &mut Context<V>) -> bool + 'static,
) {
    if live() {
        next(rx, entity, Rc::new(apply));
    }
}

fn next<V: 'static, T: Send + 'static>(rx: flume::Receiver<T>, entity: WeakEntity<V>, apply: Apply<V, T>) {
    nd::system::spawn(
        async move {
            let first = rx.recv_async().await.ok()?;
            let mut batch = vec![first];
            batch.extend(rx.try_iter().take(BATCH));
            Some((batch, rx))
        },
        move |out, cx| {
            let Some((batch, rx)) = out else { return };
            let more = entity.update(cx, |v, cx| apply(v, batch, cx)).unwrap_or(false);
            if more {
                next(rx, entity, apply);
            }
        },
    );
}
