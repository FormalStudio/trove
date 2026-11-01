//! `TROVE_TRACE=1` prints milestones measured from process start to stderr, for the cold
//! start measurement in `tests/real.rs`.

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

pub fn first_render() {
    if !*ENABLED || FIRST_RENDER.swap(true, Ordering::Relaxed) {
        return;
    }
    // nd-allow(clock-in-ui): wall time since process start is the measurement
    let start = START.get().copied().unwrap_or_else(Instant::now);
    eprintln!("[trace +{:.1}ms] first render", start.elapsed().as_secs_f64() * 1000.);
}
