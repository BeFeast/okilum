//! #1621: work the editor does on every frame and every caret move must not
//! grow with the length of the note. The platform asks for the caret's
//! bounds (IME candidate window) on every paint, and every caret move clips
//! the offset to a grapheme boundary. Both flattened the whole note into a
//! string and walked it from the start, so typing in a long note in Source
//! or Live Preview lost time on every frame.
use ::core::prelude::v1::test;
use gpui::*;
use gpui_component::{
    input::{
        projection::{
            ActiveSource, ConcealBias, ProjectedByte, ProjectionProvider, ProjectionStyle,
            SourceByte, SourceProjection, SourceSnapshot,
        },
        Editor, EditorState, WrappingIndent,
    },
    Root,
};
use std::{sync::Arc, time::Instant};

/// Source shown as is, with the exact-source bridge on, as Source and Live
/// Preview both run it.
struct Identity;
struct IdentityProjection(SourceSnapshot);
impl ProjectionProvider for Identity {
    fn compose(
        &self,
        source: &SourceSnapshot,
        _: &ActiveSource,
    ) -> Option<Arc<dyn SourceProjection>> {
        Some(Arc::new(IdentityProjection(source.clone())))
    }
}
impl SourceProjection for IdentityProjection {
    fn source(&self) -> &SourceSnapshot {
        &self.0
    }
    fn text(&self) -> &str {
        &self.0.text
    }
    fn styles(&self) -> &[ProjectionStyle] {
        &[]
    }
    fn to_projected(&self, source: SourceByte, _: ConcealBias) -> Option<ProjectedByte> {
        (source.0 <= self.0.text.len()).then_some(ProjectedByte(source.0))
    }
    fn to_source(&self, projected: ProjectedByte, _: ConcealBias) -> Option<SourceByte> {
        (projected.0 <= self.0.text.len()).then_some(SourceByte(projected.0))
    }
}

struct Surface(Entity<EditorState>);
impl Render for Surface {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(Editor::new(&self.0).size_full())
    }
}

/// The note's last lines: a CRLF line, an emoji outside the BMP, a family
/// joined with ZWJ and a flag, so boundaries and UTF-16 offsets are real.
const TAIL: &str = "crlf line\r\nbrain 🧠 coder 👩\u{200d}💻 flag 🇮🇱 end";

/// A focused editor showing `text` with the caret at its end, drawn once.
fn editor<'a>(
    cx: &'a mut TestAppContext,
    text: &str,
) -> (Entity<EditorState>, &'a mut VisualTestContext) {
    let mut input = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let state = cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .line_number(false)
                .folding(false)
                .searchable(false)
                .soft_wrap(true)
                .wrapping_indent(WrappingIndent::None);
            state.set_value(text.to_owned(), window, cx);
            state.set_projection_provider(Some(Arc::new(Identity)), cx);
            state.focus_handle(cx).focus(window, cx);
            state
        });
        input = Some(state.clone());
        Root::new(cx.new(|_| Surface(state)), window, cx)
    });
    visual.simulate_resize(size(px(800.), px(400.)));
    visual.update(|window, _| window.activate_window());
    let input = input.unwrap();
    input.update(visual, |state, cx| {
        state.set_selected_range(text.len()..text.len(), cx)
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    (input, visual)
}

/// The caret's bounds as the platform asks for them, by UTF-16 range.
fn native_bounds(
    input: &Entity<EditorState>,
    utf16: std::ops::Range<usize>,
    visual: &mut VisualTestContext,
) -> Option<Bounds<Pixels>> {
    let bounds = input.read_with(visual, |state, _| state.input_bounds());
    visual.update(|window, cx| {
        input.update(cx, |state, cx| {
            EntityInputHandler::bounds_for_range(state, utf16, bounds, window, cx)
        })
    })
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Average time of one native caret-bounds query and of one caret step left
/// then right, with the caret at the end of `text`.
fn frame_costs(cx: &mut TestAppContext, text: &str) -> (f64, f64) {
    const ROUNDS: u32 = 40;
    let (input, visual) = editor(cx, text);
    let end = utf16_len(text);
    assert!(
        native_bounds(&input, end..end, visual).is_some(),
        "positive control: the caret at the end has bounds"
    );
    let started = Instant::now();
    for _ in 0..ROUNDS {
        native_bounds(&input, end..end, visual);
    }
    let bounds = started.elapsed().as_secs_f64() / f64::from(ROUNDS);
    let started = Instant::now();
    for _ in 0..ROUNDS {
        visual.update(|window, cx| {
            window.dispatch_keystroke(Keystroke::parse("left").unwrap(), cx);
            window.dispatch_keystroke(Keystroke::parse("right").unwrap(), cx);
        });
    }
    let steps = started.elapsed().as_secs_f64() / f64::from(ROUNDS);
    input.read_with(visual, |state, _| {
        assert_eq!(state.cursor(), text.len(), "the caret came back to the end")
    });
    (bounds, steps)
}

fn note(lines: usize) -> String {
    let body: String = (0..lines)
        .map(|i| format!("Line {i} of a long note with **bold** text and words.\n"))
        .collect();
    format!("{body}{TAIL}")
}

/// A note 400 times longer costs about the same per frame and per caret
/// move. Before #1621 both grew linearly: tens of milliseconds per query on
/// a megabyte note in a debug build.
#[gpui::test]
fn caret_bounds_and_caret_steps_do_not_grow_with_the_note(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let short = note(50);
    let long = note(20_000);
    assert!(long.len() > 1_000_000, "positive control: a megabyte note");
    let (short_bounds, short_steps) = frame_costs(cx, &short);
    let (long_bounds, long_steps) = frame_costs(cx, &long);
    eprintln!(
        "caret bounds {:.3} ms -> {:.3} ms, caret step {:.3} ms -> {:.3} ms",
        short_bounds * 1e3,
        long_bounds * 1e3,
        short_steps * 1e3,
        long_steps * 1e3
    );
    // Ten times the short note's cost plus a millisecond of noise: far below
    // the 400x of work proportional to the note, far above logarithmic work.
    assert!(
        long_bounds < short_bounds * 10. + 1e-3,
        "caret bounds: {short_bounds}s on the short note, {long_bounds}s on the long one"
    );
    assert!(
        long_steps < short_steps * 10. + 1e-3,
        "caret steps: {short_steps}s on the short note, {long_steps}s on the long one"
    );
}

/// The native bridge stays exact: offsets inside a surrogate pair or past
/// the end have no bounds, and caret steps never split a grapheme or a CRLF,
/// on the last line of a long note as on a short one.
#[gpui::test]
fn native_ranges_and_caret_steps_stay_exact_on_long_notes(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    for lines in [0, 5_000] {
        let text = note(lines);
        let (input, visual) = editor(cx, &text);
        let end = utf16_len(&text);
        let brain = utf16_len(&text[..text.find('🧠').unwrap()]);
        assert!(native_bounds(&input, brain..brain, visual).is_some());
        assert!(
            native_bounds(&input, brain + 1..brain + 1, visual).is_none(),
            "inside a surrogate pair"
        );
        assert!(native_bounds(&input, brain + 2..brain + 2, visual).is_some());
        assert!(native_bounds(&input, end + 1..end + 1, visual).is_none());
        assert!(
            native_bounds(&input, end..brain, visual).is_none(),
            "reversed"
        );
        // Step left from the end across the whole tail.
        let tail = text.len() - TAIL.len();
        let mut stops = vec![text.len()];
        loop {
            visual.update(|window, cx| {
                window.dispatch_keystroke(Keystroke::parse("left").unwrap(), cx);
            });
            let at = input.read_with(visual, |state, _| state.cursor());
            if at < tail || at == *stops.last().unwrap() {
                break;
            }
            stops.push(at);
        }
        let expected: Vec<usize> = {
            use unicode_segmentation::UnicodeSegmentation;
            let mut ends: Vec<usize> = TAIL.grapheme_indices(true).map(|(i, _)| tail + i).collect();
            ends.push(text.len());
            ends.reverse();
            ends
        };
        assert_eq!(
            stops, expected,
            "{lines} lines: caret stops are the grapheme boundaries"
        );
        let crlf = text.find("\r\n").unwrap();
        assert!(!stops.contains(&(crlf + 1)), "never between CR and LF");
    }
}
