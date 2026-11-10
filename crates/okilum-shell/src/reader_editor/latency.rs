//! Sparse, content-free observation after the frame that follows a mutation.
use super::*;

#[derive(Default)]
struct Samples {
    count: u64,
    slow: u64,
    sum_ms: f64,
    max_ms: f64,
    renders: u64,
    max_renders: u64,
    since: Option<std::time::Instant>,
    last_slow: Option<std::time::Instant>,
}

pub(super) fn record(
    trace: &reader_diagnostics::Trace,
    stamp: SourceStamp,
    bytes: usize,
    elapsed: std::time::Duration,
    renders: u64,
) {
    static SAMPLES: std::sync::Mutex<Samples> = std::sync::Mutex::new(Samples {
        count: 0,
        slow: 0,
        sum_ms: 0.,
        max_ms: 0.,
        renders: 0,
        max_renders: 0,
        since: None,
        last_slow: None,
    });
    let ms = elapsed.as_secs_f64() * 1000.;
    let now = std::time::Instant::now();
    let mut samples = SAMPLES.lock().unwrap_or_else(|e| e.into_inner());
    let since = *samples.since.get_or_insert(now);
    samples.count += 1;
    samples.slow += u64::from(ms > 32.);
    samples.sum_ms += ms;
    samples.max_ms = samples.max_ms.max(ms);
    samples.renders += renders;
    samples.max_renders = samples.max_renders.max(renders);
    // One detailed slow observation per second, plus an aggregate. A typing
    // burst must not evict panic/save/index evidence from feedback's 250 lines.
    if ms > 32.
        && samples
            .last_slow
            .is_none_or(|last| now.duration_since(last).as_secs() >= 1)
    {
        samples.last_slow = Some(now);
        trace.event(
            "source_mutation_frame",
            serde_json::json!({
                "duration_ms":ms, "bytes":bytes, "document":stamp.document,
                "generation":stamp.generation, "reader_renders":renders,
                "scope":"mutation-through-painted-frame; process-wide render counter"
            }),
        );
    }
    if samples.count >= 32 || now.duration_since(since).as_secs() >= 2 {
        trace.event(
            "source_mutation_frames",
            serde_json::json!({
                "count":samples.count, "slow_over_32_ms":samples.slow,
                "mean_ms":samples.sum_ms / samples.count as f64, "max_ms":samples.max_ms,
                "mean_reader_renders":samples.renders as f64 / samples.count as f64,
                "max_reader_renders":samples.max_renders,
                "scope":"mutation-through-painted-frame; process-wide overlapping-inclusive"
            }),
        );
        let last_slow = samples.last_slow;
        *samples = Samples {
            since: Some(now),
            last_slow,
            ..Default::default()
        };
    }
}
