//! Opt-in large-note performance probe (#653). Inert unless
//! `TESSERA_READER_PERF_PROBE` names an output file; ordinary launches never
//! construct it. `scripts/benchmarks/large-note.py` drives it headlessly.
//!
//! Once the first document paints, the probe scrolls the Reader one fixed step
//! per frame, then jumps to far positions, and records the CPU time of every
//! frame from `Reader::render` to the end of the Reader's paint. It writes one
//! JSON report and quits. Presentation and GPU time are not included.
use gpui::{px, Pixels};
use std::{path::PathBuf, time::Instant};

/// Environment variable naming the JSON report the probe writes.
pub(crate) const ENV: &str = "TESSERA_READER_PERF_PROBE";
/// Pixels per scripted scroll step: about one mouse-wheel notch.
const STEP: Pixels = px(96.);
/// Frames after the first document paint that are not timed.
const SETTLE_FRAMES: usize = 3;
const SCROLL_FRAMES: usize = 240;
/// Far jumps as fractions of the block list, each followed by settle frames.
const JUMPS: [f32; 4] = [0.5, 0.95, 0.25, 0.75];
const FRAMES_PER_JUMP: usize = 8;

/// What the Reader does before its next frame.
pub(crate) enum Step {
    /// Nothing; the probe is finished or still waiting for the document.
    Idle,
    /// Scroll the document by this distance.
    ScrollBy(Pixels),
    /// Scroll the document so this block is at the top.
    Jump(usize),
    /// Write the report and quit.
    Finish,
}

enum Phase {
    WaitingForDocument,
    Settling(usize),
    Scrolling(usize),
    Jumping { jump: usize, frame: usize },
    Done,
}

pub(crate) struct Probe {
    output: PathBuf,
    phase: Phase,
    render_started: Option<Instant>,
    open_ms: f64,
    first_frame_ms: f64,
    first_frame_blocks: usize,
    scroll_frames: Vec<f64>,
    jump_frames: Vec<f64>,
    start_item: usize,
    end_item: usize,
    jump_items: Vec<(usize, usize)>,
}

impl Probe {
    pub(crate) fn from_env() -> Option<Self> {
        let output = std::env::var_os(ENV).filter(|value| !value.is_empty())?;
        Some(Self {
            output: PathBuf::from(output),
            phase: Phase::WaitingForDocument,
            render_started: None,
            open_ms: 0.,
            first_frame_ms: 0.,
            first_frame_blocks: 0,
            scroll_frames: Vec::new(),
            jump_frames: Vec::new(),
            start_item: 0,
            end_item: 0,
            jump_items: Vec::new(),
        })
    }

    /// Called at the start of `Reader::render`.
    pub(crate) fn render_started(&mut self) {
        self.render_started = Some(Instant::now());
    }

    /// Called at the end of the Reader's paint. `blocks` is the document's
    /// block count and `top` the first visible block; `elapsed_ms` is the
    /// launch clock. Returns what to do before the next frame.
    pub(crate) fn painted(&mut self, blocks: usize, top: usize, elapsed_ms: f64) -> Step {
        let Some(started) = self.render_started.take() else {
            return Step::Idle;
        };
        let frame_ms = started.elapsed().as_secs_f64() * 1000.;
        match &mut self.phase {
            Phase::WaitingForDocument => {
                if blocks == 0 {
                    return Step::Idle;
                }
                self.open_ms = elapsed_ms;
                self.first_frame_ms = frame_ms;
                self.first_frame_blocks = blocks;
                self.phase = Phase::Settling(0);
                Step::ScrollBy(px(0.))
            }
            Phase::Settling(frame) => {
                *frame += 1;
                if *frame < SETTLE_FRAMES {
                    return Step::ScrollBy(px(0.));
                }
                self.start_item = top;
                self.phase = Phase::Scrolling(0);
                Step::ScrollBy(STEP)
            }
            Phase::Scrolling(frame) => {
                self.scroll_frames.push(frame_ms);
                *frame += 1;
                if *frame < SCROLL_FRAMES {
                    return Step::ScrollBy(STEP);
                }
                self.end_item = top;
                self.phase = Phase::Jumping { jump: 0, frame: 0 };
                Step::Jump(jump_target(blocks, 0))
            }
            Phase::Jumping { jump, frame } => {
                self.jump_frames.push(frame_ms);
                if *frame == 0 {
                    self.jump_items.push((jump_target(blocks, *jump), top));
                }
                *frame += 1;
                if *frame < FRAMES_PER_JUMP {
                    return Step::ScrollBy(STEP);
                }
                *jump += 1;
                *frame = 0;
                if *jump < JUMPS.len() {
                    return Step::Jump(jump_target(blocks, *jump));
                }
                self.phase = Phase::Done;
                Step::Finish
            }
            Phase::Done => Step::Idle,
        }
    }

    /// Write the JSON report. Positive controls let the harness reject a run
    /// that never painted the document or never moved it.
    pub(crate) fn write_report(
        &self,
        outline_rows: usize,
        outline_height: f32,
    ) -> std::io::Result<()> {
        let report = serde_json::json!({
            "open_ms": self.open_ms,
            "first_frame_ms": self.first_frame_ms,
            "blocks": self.first_frame_blocks,
            "outline_rows": outline_rows,
            "outline_height_px": outline_height,
            "step_px": f32::from(STEP),
            "scroll": stats(&self.scroll_frames),
            "jump": stats(&self.jump_frames),
            "scroll_start_item": self.start_item,
            "scroll_end_item": self.end_item,
            "jump_items": self.jump_items,
        });
        std::fs::write(&self.output, serde_json::to_vec_pretty(&report)?)
    }
}

fn jump_target(blocks: usize, jump: usize) -> usize {
    ((blocks.saturating_sub(1)) as f32 * JUMPS[jump]) as usize
}

fn stats(frames: &[f64]) -> serde_json::Value {
    let mut sorted = frames.to_vec();
    sorted.sort_by(f64::total_cmp);
    let pick = |q: f64| {
        sorted
            .get(((sorted.len() as f64 - 1.) * q).round() as usize)
            .copied()
            .unwrap_or(0.)
    };
    serde_json::json!({
        "frames": sorted.len(),
        "mean_ms": sorted.iter().sum::<f64>() / sorted.len().max(1) as f64,
        "p50_ms": pick(0.5),
        "p95_ms": pick(0.95),
        "max_ms": sorted.last().copied().unwrap_or(0.),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe() -> Probe {
        Probe {
            output: PathBuf::new(),
            phase: Phase::WaitingForDocument,
            render_started: None,
            open_ms: 0.,
            first_frame_ms: 0.,
            first_frame_blocks: 0,
            scroll_frames: Vec::new(),
            jump_frames: Vec::new(),
            start_item: 0,
            end_item: 0,
            jump_items: Vec::new(),
        }
    }

    #[test]
    fn probe_waits_for_a_document_then_scrolls_jumps_and_finishes() {
        let mut probe = probe();
        // A paint without a matching render start is not a frame.
        assert!(matches!(probe.painted(10, 0, 1.), Step::Idle));
        probe.render_started();
        assert!(matches!(probe.painted(0, 0, 1.), Step::Idle));
        probe.render_started();
        assert!(matches!(probe.painted(1000, 0, 42.), Step::ScrollBy(_)));
        assert_eq!(probe.open_ms, 42.);
        let mut steps = 0;
        loop {
            probe.render_started();
            match probe.painted(1000, steps, 50.) {
                Step::Finish => break,
                Step::Idle => panic!("probe stalled"),
                _ => steps += 1,
            }
        }
        assert_eq!(probe.scroll_frames.len(), SCROLL_FRAMES);
        assert_eq!(probe.jump_frames.len(), JUMPS.len() * FRAMES_PER_JUMP);
        assert_eq!(probe.jump_items.len(), JUMPS.len());
        assert_eq!(probe.jump_items[0].0, 499);
        assert!(probe.end_item > probe.start_item);
        probe.render_started();
        assert!(matches!(probe.painted(1000, 0, 60.), Step::Idle));
    }

    #[test]
    fn stats_report_percentiles() {
        let frames: Vec<f64> = (1..=100).map(f64::from).collect();
        let stats = stats(&frames);
        assert_eq!(stats["frames"], 100);
        assert_eq!(stats["max_ms"], 100.);
        assert_eq!(stats["p95_ms"], 95.);
    }
}
