//! Bounded observation windows over the application's platform-text proxy.
//! Counters are process-wide: overlapping windows and other UI work are included.
use std::{
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
    time::Instant,
};

const NAMES: [&str; 10] = [
    "font_lookup",
    "font_metrics",
    "glyph_lookup",
    "line_layout",
    "platform_shape",
    "glyph_raster",
    "ui_render",
    "backlink_titles",
    "reader_ui_sync",
    "glyph_bitmap",
];
#[derive(Clone, Copy)]
pub(crate) enum Work {
    FontLookup,
    FontMetrics,
    GlyphLookup,
    LineLayout,
    PlatformShape,
    GlyphRaster,
    UiRender,
    BacklinkTitles,
    ReaderUiSync,
    GlyphBitmap,
}
static ACTIVE: AtomicUsize = AtomicUsize::new(0);
static CALLS: [AtomicU64; NAMES.len()] = [const { AtomicU64::new(0) }; NAMES.len()];
static NANOS: [AtomicU64; NAMES.len()] = [const { AtomicU64::new(0) }; NAMES.len()];

pub(crate) struct Probe {
    calls: [u64; NAMES.len()],
    nanos: [u64; NAMES.len()],
}
impl Probe {
    pub(crate) fn start() -> Self {
        let probe = Self {
            calls: std::array::from_fn(|i| CALLS[i].load(Ordering::Relaxed)),
            nanos: std::array::from_fn(|i| NANOS[i].load(Ordering::Relaxed)),
        };
        ACTIVE.fetch_add(1, Ordering::Relaxed);
        probe
    }
    pub(crate) fn report(&self) -> serde_json::Value {
        let work: serde_json::Map<String, serde_json::Value> = NAMES.iter().enumerate().map(|(i, name)| (
            (*name).to_owned(), serde_json::json!({
                "calls": CALLS[i].load(Ordering::Relaxed).saturating_sub(self.calls[i]),
                "duration_ms": NANOS[i].load(Ordering::Relaxed).saturating_sub(self.nanos[i]) as f64 / 1_000_000.,
            })
        )).collect();
        serde_json::json!({"os": std::env::consts::OS, "scope": "process-wide-overlapping-inclusive", "work": work})
    }
}
impl Drop for Probe {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::Relaxed);
    }
}
pub(crate) struct Timer {
    work: Work,
    started: Instant,
}
pub(crate) fn time(work: Work) -> Option<Timer> {
    (ACTIVE.load(Ordering::Relaxed) != 0).then(|| Timer {
        work,
        started: Instant::now(),
    })
}
impl Drop for Timer {
    fn drop(&mut self) {
        let i = self.work as usize;
        NANOS[i].fetch_add(
            self.started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
        CALLS[i].fetch_add(1, Ordering::Relaxed);
    }
}
