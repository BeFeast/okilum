//! Opt-in large-note Reader probe (#653). Inert unless `OKILUM_READER_PERF_PROBE`
//! names an output file; ordinary launches never construct it.
//! `scripts/perf/large-note/bench.py` drives it under Xvfb.
//!
//! Once the first document paints, the probe scrolls the Reader one fixed step
//! per frame, then jumps through «On this page» to far headings, then (#3154)
//! scrolls on a wall-clock timer while counting every painted frame and all
//! text layout work until the scenario settles, and records
//! the CPU time of every frame from `Reader::render` to the end of the Reader
//! body's paint. It writes one JSON report and quits. Presentation and GPU time
//! are not included.
use gpui::{px, Pixels};
use gpui_base::text::layout_counters::{self, LayoutCounts};
use std::{path::PathBuf, time::Instant};

/// Environment variable naming the JSON report the probe writes.
pub(crate) const ENV: &str = "OKILUM_READER_PERF_PROBE";
/// Pixels per scripted scroll step: about one mouse-wheel notch.
pub(crate) const STEP: Pixels = px(96.);
/// Frames after the first document paint that are not timed.
const SETTLE_FRAMES: usize = 3;
const SCROLL_FRAMES: usize = 240;
/// Far outline jumps as fractions of the outline. Each is timed over this many
/// frames without other input; the landing is read on the last of them.
const JUMPS: [f32; 4] = [0.5, 0.95, 0.25, 0.75];
const FRAMES_PER_JUMP: usize = 8;
/// The timed phase (#3154): scroll steps sent by a wall-clock timer, not by
/// paint callbacks, then a quiet settle. Every frame painted and all text
/// layout work in that window are counted, so a decoration that caused an
/// extra repaint or layout pass shows up in the counts.
pub(crate) const TIMED_STEPS: usize = 120;
pub(crate) const TIMED_INTERVAL_MS: u64 = 16;
pub(crate) const TIMED_SETTLE_MS: u64 = 1000;

/// What the Reader does before its next frame.
#[derive(Debug, PartialEq)]
pub(crate) enum Step {
    /// Nothing; the probe is finished or still waiting for the document.
    Idle,
    /// Scroll the document by this distance.
    ScrollBy(Pixels),
    /// Open this «On this page» row, as a click on it does.
    Jump(usize),
    /// Start the timed phase: the Reader sends `TIMED_STEPS` scroll steps of
    /// `STEP` from a timer, waits `TIMED_SETTLE_MS`, then calls
    /// [`Probe::finish_timed`].
    StartTimed,
}

/// One outline jump: the row opened, its heading's block, the block on top
/// after the jump's frames, and the frame (0 = the first after the click) in
/// which that block first was on top.
#[derive(Debug, PartialEq)]
struct Jump {
    row: usize,
    target: usize,
    top: usize,
    landed_frame: Option<usize>,
}

#[derive(Debug, PartialEq)]
enum Phase {
    WaitingForDocument,
    Settling(usize),
    Scrolling(usize),
    Jumping { jump: usize, frame: usize },
    Timed,
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
    jumps: Vec<Jump>,
    /// (outline row, its target block, first frame with the block on top)
    pending_jump: Option<(usize, usize, Option<usize>)>,
    /// Text layout work per phase, counted by the TextView and its inline
    /// elements themselves (#3154), not by this probe's frame count.
    phase_layout_start: LayoutCounts,
    scroll_layout: LayoutCounts,
    jump_layout: LayoutCounts,
    /// Frames painted during the timed phase, whatever caused them.
    timed_frames: Vec<f64>,
    timed_layout: LayoutCounts,
}

impl Probe {
    pub(crate) fn from_env() -> Option<Self> {
        let output = std::env::var_os(ENV).filter(|value| !value.is_empty())?;
        Some(Self::new(PathBuf::from(output)))
    }

    fn new(output: PathBuf) -> Self {
        Self {
            output,
            phase: Phase::WaitingForDocument,
            render_started: None,
            open_ms: 0.,
            first_frame_ms: 0.,
            first_frame_blocks: 0,
            scroll_frames: Vec::new(),
            jump_frames: Vec::new(),
            start_item: 0,
            end_item: 0,
            jumps: Vec::new(),
            pending_jump: None,
            phase_layout_start: LayoutCounts::default(),
            scroll_layout: LayoutCounts::default(),
            jump_layout: LayoutCounts::default(),
            timed_frames: Vec::new(),
            timed_layout: LayoutCounts::default(),
        }
    }

    /// Ends the timed phase once its last step has settled; the Reader then
    /// writes the report and quits.
    pub(crate) fn finish_timed(&mut self) {
        if self.phase == Phase::Timed {
            self.timed_layout = layout_counters::snapshot().since(self.phase_layout_start);
            self.phase = Phase::Done;
        }
    }

    /// Called at the start of `Reader::render`.
    pub(crate) fn render_started(&mut self) {
        self.render_started = Some(Instant::now());
    }

    /// Called at the end of the Reader body's paint. `blocks` is the document's
    /// block count (0 until it is ready), `top` the first visible block and
    /// `outline` the targets of the «On this page» rows; `elapsed_ms` is the
    /// launch clock. Returns what to do before the next frame.
    pub(crate) fn painted(
        &mut self,
        blocks: usize,
        top: usize,
        outline: &[usize],
        elapsed_ms: f64,
    ) -> Step {
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
                self.phase_layout_start = layout_counters::snapshot();
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
                let now = layout_counters::snapshot();
                self.scroll_layout = now.since(self.phase_layout_start);
                self.phase_layout_start = now;
                self.phase = Phase::Jumping { jump: 0, frame: 0 };
                self.jump(0, outline)
            }
            Phase::Jumping { jump, frame } => {
                self.jump_frames.push(frame_ms);
                if let Some((_, target, landed)) = &mut self.pending_jump {
                    if landed.is_none() && top == *target {
                        *landed = Some(*frame);
                    }
                }
                *frame += 1;
                if *frame < FRAMES_PER_JUMP {
                    return Step::ScrollBy(px(0.));
                }
                if let Some((row, target, landed_frame)) = self.pending_jump.take() {
                    self.jumps.push(Jump {
                        row,
                        target,
                        top,
                        landed_frame,
                    });
                }
                let next = *jump + 1;
                if next < JUMPS.len() {
                    self.phase = Phase::Jumping {
                        jump: next,
                        frame: 0,
                    };
                    return self.jump(next, outline);
                }
                let now = layout_counters::snapshot();
                self.jump_layout = now.since(self.phase_layout_start);
                self.phase_layout_start = now;
                self.phase = Phase::Timed;
                Step::StartTimed
            }
            Phase::Timed => {
                // Counted, never driven: input comes from the timer.
                self.timed_frames.push(frame_ms);
                Step::Idle
            }
            Phase::Done => Step::Idle,
        }
    }

    fn jump(&mut self, jump: usize, outline: &[usize]) -> Step {
        let row = (outline.len().saturating_sub(1) as f32 * JUMPS[jump]) as usize;
        match outline.get(row) {
            Some(&block) => {
                self.pending_jump = Some((row, block, None));
                Step::Jump(row)
            }
            // No outline: the report's controls reject the run.
            None => Step::ScrollBy(px(0.)),
        }
    }

    /// Write the JSON report. Positive controls let the harness reject a run
    /// that never painted the document, never moved it or never jumped.
    /// `theme` names the live theme, mode and syntax palette of the run.
    pub(crate) fn write_report(
        &self,
        outline_rows: usize,
        theme: serde_json::Value,
    ) -> std::io::Result<()> {
        let report = serde_json::json!({
            "theme": theme,
            "open_ms": self.open_ms,
            "first_frame_ms": self.first_frame_ms,
            "blocks": self.first_frame_blocks,
            "outline_rows": outline_rows,
            "step_px": f32::from(STEP),
            "scroll": stats(&self.scroll_frames),
            "jump": stats(&self.jump_frames),
            "layout": {
                "scroll": counts(self.scroll_layout),
                "jump": counts(self.jump_layout),
            },
            "timed": {
                "input_steps": TIMED_STEPS,
                "interval_ms": TIMED_INTERVAL_MS,
                "settle_ms": TIMED_SETTLE_MS,
                "frame": stats(&self.timed_frames),
                "layout": counts(self.timed_layout),
            },
            "scroll_start_item": self.start_item,
            "scroll_end_item": self.end_item,
            "jumps": self.jumps.iter().map(|jump| serde_json::json!({
                "outline_row": jump.row, "target_block": jump.target, "top_block": jump.top,
                "landed_frame": jump.landed_frame,
            })).collect::<Vec<_>>(),
        });
        std::fs::write(&self.output, serde_json::to_vec_pretty(&report)?)
    }
}

fn counts(counts: LayoutCounts) -> serde_json::Value {
    serde_json::json!({
        "text_view_layouts": counts.text_view_layouts,
        "inline_layouts": counts.inline_layouts,
        "inline_prepaints": counts.inline_prepaints,
    })
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

    #[test]
    fn probe_waits_for_a_document_then_scrolls_jumps_and_finishes() {
        let mut probe = Probe::new(PathBuf::new());
        let outline: Vec<usize> = (0..1000).map(|i| i * 2).collect();
        // A paint without a matching render start is not a frame.
        assert_eq!(probe.painted(10, 0, &outline, 1.), Step::Idle);
        probe.render_started();
        assert_eq!(probe.painted(0, 0, &outline, 1.), Step::Idle);
        probe.render_started();
        assert_eq!(
            probe.painted(2000, 0, &outline, 42.),
            Step::ScrollBy(px(0.))
        );
        assert_eq!(probe.open_ms, 42.);
        let mut steps = 0;
        let mut jumps = Vec::new();
        let mut top = 0;
        // The Reader lands an outline jump one frame after the click.
        let mut landing = None;
        loop {
            probe.render_started();
            let step = probe.painted(2000, top, &outline, 50.);
            if let Some(block) = landing.take() {
                top = block;
            }
            match step {
                Step::StartTimed => break,
                Step::Idle => panic!("probe stalled"),
                Step::Jump(row) => {
                    jumps.push(row);
                    landing = Some(outline[row]);
                }
                Step::ScrollBy(distance) => {
                    steps += 1;
                    if distance > px(0.) {
                        top += 1;
                    }
                }
            }
        }
        assert!(steps > SCROLL_FRAMES);
        assert_eq!(probe.scroll_frames.len(), SCROLL_FRAMES);
        assert_eq!(probe.jump_frames.len(), JUMPS.len() * FRAMES_PER_JUMP);
        assert_eq!(jumps, vec![499, 949, 249, 749]);
        assert_eq!(
            probe.jumps,
            jumps
                .iter()
                .map(|&row| Jump {
                    row,
                    target: outline[row],
                    top: outline[row],
                    landed_frame: Some(1),
                })
                .collect::<Vec<_>>()
        );
        assert!(probe.end_item > probe.start_item);
        // Timed phase: every painted frame is counted, none drives input,
        // however many the timer's steps cause.
        for _ in 0..TIMED_STEPS + 7 {
            probe.render_started();
            assert_eq!(probe.painted(2000, top, &outline, 60.), Step::Idle);
        }
        assert_eq!(probe.timed_frames.len(), TIMED_STEPS + 7);
        probe.finish_timed();
        assert_eq!(probe.phase, Phase::Done);
        probe.render_started();
        assert_eq!(probe.painted(2000, 0, &outline, 60.), Step::Idle);
    }

    #[test]
    fn a_note_without_an_outline_records_no_jumps() {
        let mut probe = Probe::new(PathBuf::new());
        let mut frames = 0;
        loop {
            probe.render_started();
            match probe.painted(50, 0, &[], 1.) {
                Step::StartTimed => break,
                Step::Jump(_) => panic!("jumped without an outline"),
                _ => frames += 1,
            }
            assert!(frames < 10_000);
        }
        assert!(probe.jumps.is_empty());
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
