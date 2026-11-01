use std::collections::HashSet;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

use ignore::WalkState;

use crate::matcher::{Matcher, Term};
use crate::mounts::Mount;
use crate::scan::{Ranked, TopK, rank_key};
use crate::text::{fold, kind_code};
use crate::walk::{Prune, mtime_of, walker};

const EMIT_EVERY: Duration = Duration::from_millis(50);

#[derive(Clone, Copy)]
pub(crate) struct LiveOpts {
    pub max_depth: Option<usize>,
    pub budget: Option<u64>,
    /// Say so when the depth or the budget cut the walk short.
    pub report_cut: bool,
    pub limit: usize,
}

pub(crate) struct LiveResult {
    pub scanned: u64,
    pub truncated: bool,
}

/// Walks `scope` with the parallel walker and the query's matcher, calling `emit` with the best
/// list so far (merged with `base`, deduplicated by path) at most every 50 ms and once at the
/// end. `emit` returning false stops the walk.
pub(crate) fn search(
    scope: &Path,
    m: &Matcher,
    opts: LiveOpts,
    base: &[Ranked],
    mounts: &[Mount],
    cancel: &AtomicBool,
    mut emit: impl FnMut(Vec<Ranked>) -> bool,
) -> LiveResult {
    let stop = AtomicBool::new(false);
    let scanned = AtomicU64::new(0);
    let truncated = AtomicBool::new(false);
    let within = m.within_left(&fold(scope.file_name().map_or(&b""[..], |n| n.as_bytes())));
    let (tx, rx) = flume::unbounded::<Ranked>();
    let prune = Prune::new(scope, mounts, m.allow_hidden);
    let base_sub = base.iter().filter(|r| !r.fuzzy).count();
    let limit = opts.limit;

    std::thread::scope(|s| {
        let walk = walker(scope, prune, opts.max_depth);
        let (stop, scanned, truncated, within) = (&stop, &scanned, &truncated, &within);
        s.spawn(move || {
            walk.run(|| {
                let tx = tx.clone();
                Box::new(move |res| {
                    if stop.load(Relaxed) || cancel.load(Relaxed) {
                        return WalkState::Quit;
                    }
                    let Ok(e) = res else { return WalkState::Continue };
                    if e.depth() == 0 {
                        return WalkState::Continue;
                    }
                    let n = scanned.fetch_add(1, Relaxed) + 1;
                    if opts.budget.is_some_and(|b| n > b) {
                        truncated.store(opts.report_cut, Relaxed);
                        stop.store(true, Relaxed);
                        return WalkState::Quit;
                    }
                    let is_dir = e.file_type().is_some_and(|t| t.is_dir());
                    if opts.report_cut && is_dir && opts.max_depth == Some(e.depth()) {
                        truncated.store(true, Relaxed);
                    }
                    if let Some(r) = rate(scope, &e, is_dir, m, within) {
                        let _ = tx.send(r);
                    }
                    WalkState::Continue
                })
            });
        });

        let mut sub: TopK<Ranked> = TopK::new(limit);
        let mut fz: TopK<Ranked> = TopK::new(limit);
        let mut sub_count = base_sub;
        let mut dirty = false;
        let mut last = Instant::now();
        loop {
            match rx.recv_timeout(Duration::from_millis(10)) {
                Ok(r) => {
                    if r.fuzzy {
                        fz.push(r);
                    } else {
                        sub_count += 1;
                        sub.push(r);
                    }
                    dirty = true;
                }
                Err(flume::RecvTimeoutError::Timeout) => {}
                Err(flume::RecvTimeoutError::Disconnected) => break,
            }
            if cancel.load(Relaxed) {
                stop.store(true, Relaxed);
            }
            if dirty && last.elapsed() >= EMIT_EVERY && !stop.load(Relaxed) {
                if !emit(merge(&sub, &fz, sub_count, limit, base)) {
                    stop.store(true, Relaxed);
                }
                dirty = false;
                last = Instant::now();
            }
        }
        if dirty && !cancel.load(Relaxed) {
            emit(merge(&sub, &fz, sub_count, limit, base));
        }
    });
    LiveResult { scanned: scanned.load(Relaxed), truncated: truncated.load(Relaxed) }
}

fn rate(scope: &Path, e: &ignore::DirEntry, is_dir: bool, m: &Matcher, within: &[Vec<u8>]) -> Option<Ranked> {
    let name = e.file_name().as_bytes();
    let lower = fold(name);
    let kind = if m.kinds != 0 && !is_dir { kind_code(name) } else { 0 };
    if !m.cheap_ok(&lower, is_dir, kind) {
        return None;
    }
    let mut md = None;
    if m.needs_meta() {
        let x = e.metadata().ok()?;
        if !m.meta_ok(if is_dir { 0 } else { x.len() }, u64::from(mtime_of(&x))) {
            return None;
        }
        md = Some(x);
    }
    let rel = e.path().strip_prefix(scope).unwrap_or(e.path());
    let above: Vec<&[u8]> = rel
        .parent()
        .map(|p| {
            p.components()
                .filter_map(|c| match c {
                    Component::Normal(n) => Some(n.as_bytes()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let folded: Vec<Vec<u8>> = above.iter().map(|a| fold(a)).collect();
    if !within.iter().all(|w| folded.iter().any(|f| f == w)) {
        return None;
    }
    let in_ancestor = |t: &Term| above.iter().zip(&folded).any(|(a, f)| t.in_ancestor(a, f));
    let (q, fuzzy) = match m.eval(name, &lower, is_dir, false, in_ancestor) {
        Some(q) => (q, false),
        None if m.has_fuzzy() => (m.eval(name, &lower, is_dir, true, in_ancestor)?, true),
        None => return None,
    };
    let mtime = match md {
        Some(x) => mtime_of(&x),
        None => e.metadata().map_or(0, |x| mtime_of(&x)),
    };
    Some(Ranked { key: rank_key(q, e.depth(), mtime), path: e.path().to_path_buf(), is_dir, fuzzy })
}

/// Plain matches; subsequence ones only while there are fewer plain ones than the limit.
fn merge(sub: &TopK<Ranked>, fz: &TopK<Ranked>, sub_count: usize, limit: usize, base: &[Ranked]) -> Vec<Ranked> {
    let mut all: Vec<Ranked> = sub.iter().cloned().collect();
    if sub_count < limit {
        all.extend(fz.iter().cloned());
    }
    all.extend(base.iter().cloned());
    all.sort_unstable_by(|a, b| b.cmp(a));
    let mut seen = HashSet::with_capacity(all.len());
    all.retain(|r| seen.insert(r.path.clone()));
    all.truncate(limit);
    all
}
