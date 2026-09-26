//! Frame-cost instrumentation, off unless `MIND_MAP_DEBUG=1`.
//!
//! The paint pass runs inside a canvas callback that has no path back to the
//! view, so the numbers land in atomics and the canvas corner reads them.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

static PAINT_US: AtomicU64 = AtomicU64::new(0);
static PATHS: AtomicU64 = AtomicU64::new(0);
static DRAWN: AtomicU64 = AtomicU64::new(0);
static TOTAL: AtomicU64 = AtomicU64::new(0);
static BUILD_US: AtomicU64 = AtomicU64::new(0);

/// Exponential smoothing on the paint time, in 1/1000ths — a single slow frame
/// shouldn't make the readout unreadable.
const SMOOTH: u64 = 200;

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var("MIND_MAP_DEBUG")
            .map(|v| v != "0" && !v.is_empty())
            .unwrap_or(false)
    })
}

/// Times a paint pass and records what it drew. A no-op when debug is off, so
/// it can sit in the hot path unconditionally.
pub struct Frame {
    start: Option<Instant>,
}

impl Frame {
    pub fn start() -> Self {
        Self {
            start: enabled().then(Instant::now),
        }
    }

    /// `drawn` / `total` are nodes actually painted vs nodes in the map, so a
    /// culling regression shows up immediately.
    pub fn end(self, paths: u64, drawn: u64, total: u64) {
        let Some(t0) = self.start else { return };
        let us = t0.elapsed().as_micros() as u64;
        let prev = PAINT_US.load(Ordering::Relaxed);
        let smoothed = if prev == 0 {
            us * 1000
        } else {
            (prev * (1000 - SMOOTH) + us * 1000 * SMOOTH) / 1000
        };
        PAINT_US.store(smoothed, Ordering::Relaxed);
        PATHS.store(paths, Ordering::Relaxed);
        DRAWN.store(drawn, Ordering::Relaxed);
        TOTAL.store(total, Ordering::Relaxed);
    }
}

/// Time spent turning the graph into a paint list, in microseconds.
pub fn record_build(us: u64) {
    let prev = BUILD_US.load(Ordering::Relaxed);
    let v = if prev == 0 { us * 1000 } else { (prev * 800 + us * 1000 * 200) / 1000 };
    BUILD_US.store(v, Ordering::Relaxed);
}

/// One-line readout for the status bar, e.g.
/// `paint 0.21ms · build 0.02ms · 104 paths · 34/2000`.
pub fn readout() -> Option<String> {
    if !enabled() {
        return None;
    }
    let us = PAINT_US.load(Ordering::Relaxed) as f64 / 1000.0;
    Some(format!(
        "paint {:.2}ms · build {:.2}ms · {} paths · {}/{}",
        us / 1000.0,
        BUILD_US.load(Ordering::Relaxed) as f64 / 1_000_000.0,
        PATHS.load(Ordering::Relaxed),
        DRAWN.load(Ordering::Relaxed),
        TOTAL.load(Ordering::Relaxed),
    ))
}
