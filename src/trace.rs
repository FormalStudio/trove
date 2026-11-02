//! `TROVE_TRACE=1` prints milestones measured from process start to stderr, for the cold
//! start measurement in `tests/real.rs` and the budgets in `tests/real_perf.rs`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, OnceLock};
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();
static ENABLED: LazyLock<bool> = LazyLock::new(|| std::env::var_os("TROVE_TRACE").is_some_and(|v| v != "0"));
static FIRST_RENDER: AtomicBool = AtomicBool::new(false);

/// First thing in `run`, so the times count from process start.
pub fn init() {
    // nd-allow(clock-in-ui): process start, before any executor exists
    START.get_or_init(Instant::now);
}

fn since_start() -> f64 {
    // nd-allow(clock-in-ui): wall time since process start is the measurement
    let start = START.get().copied().unwrap_or_else(Instant::now);
    start.elapsed().as_secs_f64() * 1000.
}

pub fn first_render() {
    if !*ENABLED || FIRST_RENDER.swap(true, Ordering::Relaxed) {
        return;
    }
    eprintln!("[trace +{:.1}ms] first render", since_start());
}

/// A milestone, every time it is reached.
pub fn mark(what: &str) {
    if *ENABLED {
        eprintln!("[trace +{:.1}ms] {what}", since_start());
    }
}

/// Runs `f` and prints how long it held the thread.
pub fn timed<T>(what: &str, f: impl FnOnce() -> T) -> T {
    if !*ENABLED {
        return f();
    }
    // nd-allow(clock-in-ui): the span is the measurement
    let t0 = Instant::now();
    let out = f();
    eprintln!("[trace +{:.1}ms] {what} took {:.2}ms", since_start(), t0.elapsed().as_secs_f64() * 1000.);
    out
}
