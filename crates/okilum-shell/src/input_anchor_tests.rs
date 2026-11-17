//! #1621: rows above an edit that change height while it is laid out (a
//! rendered block drawn as source for a frame, then rendered again) never
//! move the view. The caret moves down one row per Enter, or stays where it
//! is while the view follows it at the bottom edge, and never leaves the view.
use ::core::prelude::v1::test;
use gpui::*;
use gpui_component::{
    input::{
        projection::{
            ActiveSource, ConcealBias, LineScale, ProjectedByte, ProjectionProvider,
            ProjectionStyle, SourceByte, SourceProjection, SourceSnapshot,
        },
        Editor, EditorState, WrappingIndent,
    },
    Root,
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

/// Where the editor drew, read right after a draw.
pub(crate) struct Frame {
    pub(crate) view: Bounds<Pixels>,
    pub(crate) caret: Option<Bounds<Pixels>>,
    pub(crate) reference: Option<Pixels>,
    pub(crate) line: Pixels,
}

/// Draw one frame, as a slow machine would between a key and the next task.
pub(crate) fn draw(
    input: &Entity<EditorState>,
    reference: usize,
    visual: &mut VisualTestContext,
) -> Frame {
    visual.update(|window, cx| window.draw(cx).clear(cx));
    input.read_with(visual, |input, _| {
        let cursor = input.cursor();
        Frame {
            view: input.input_bounds(),
            caret: input.range_to_bounds(&(cursor..cursor)),
            reference: input
                .range_to_bounds(&(reference..reference))
                .map(|bounds| bounds.top()),
            line: input.cursor_layout().expect("a painted caret").1,
        }
    })
}

/// The row at `reference`, above the edit, stays where it was or moves up
/// one row as the view follows the caret; the caret moves down at most one
/// row and stays inside the view.
pub(crate) fn assert_held(before: &Frame, after: &Frame, what: &str) {
    let near = |a: Pixels, b: Pixels| (f32::from(a) - f32::from(b)).abs() < 0.5;
    let line = after.line;
    let (Some(old), Some(new)) = (before.reference, after.reference) else {
        panic!("{what}: the row above the edit left the view");
    };
    let moved = new - old;
    assert!(
        near(moved, px(0.)) || near(moved, -line),
        "{what}: the rows above the edit moved by {moved:?} (one row is {line:?})"
    );
    let caret = after
        .caret
        .unwrap_or_else(|| panic!("{what}: the caret left the view"));
    assert!(
        caret.top() >= after.view.top() - px(0.5)
            && caret.bottom() <= after.view.bottom() + px(0.5),
        "{what}: the caret {caret:?} is outside the view {:?}",
        after.view
    );
    if let Some(old) = before.caret {
        let down = caret.top() - old.top();
        assert!(
            near(down, px(0.)) || near(down, line),
            "{what}: the caret moved by {down:?} (one row is {line:?})"
        );
    }
}

const HEADINGS: usize = 12;

/// An identity projection whose first lines are headings at twice the size;
/// while `raw` is set it has no projection, so the editor lays the note out
/// as source, as Live Preview does until a background classification comes.
struct Headings {
    raw: AtomicBool,
}
struct HeadingProjection {
    source: SourceSnapshot,
    scales: Vec<LineScale>,
}
impl ProjectionProvider for Headings {
    fn compose(
        &self,
        source: &SourceSnapshot,
        _: &ActiveSource,
    ) -> Option<Arc<dyn SourceProjection>> {
        if self.raw.load(Ordering::SeqCst) {
            return None;
        }
        Some(Arc::new(HeadingProjection {
            source: source.clone(),
            scales: (0..HEADINGS)
                .map(|line| LineScale {
                    line,
                    scale: 2.,
                    hang: 0,
                })
                .collect(),
        }))
    }
}
impl SourceProjection for HeadingProjection {
    fn line_scales(&self) -> &[LineScale] {
        &self.scales
    }
    fn source(&self) -> &SourceSnapshot {
        &self.source
    }
    fn text(&self) -> &str {
        &self.source.text
    }
    fn styles(&self) -> &[ProjectionStyle] {
        &[]
    }
    fn to_projected(&self, source: SourceByte, _: ConcealBias) -> Option<ProjectedByte> {
        (source.0 <= self.source.text.len()).then_some(ProjectedByte(source.0))
    }
    fn to_source(&self, projected: ProjectedByte, _: ConcealBias) -> Option<SourceByte> {
        (projected.0 <= self.source.text.len()).then_some(SourceByte(projected.0))
    }
}

struct Surface(Entity<EditorState>);
impl Render for Surface {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(Editor::new(&self.0).size_full())
    }
}

/// An editor at the top of the note with the caret at `caret`.
fn editor_at<'a>(
    cx: &'a mut TestAppContext,
    text: &str,
    provider: Arc<dyn ProjectionProvider>,
    caret: usize,
) -> (Entity<EditorState>, &'a mut VisualTestContext) {
    cx.update(crate::test_init);
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
            state.set_projection_provider(Some(provider), cx);
            state.focus_handle(cx).focus(window, cx);
            state
        });
        input = Some(state.clone());
        let view = cx.new(|_| Surface(state));
        Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(800.), px(400.)));
    visual.update(|window, _| window.activate_window());
    visual.run_until_parked();
    let input = input.unwrap();
    input.update(visual, |state, cx| {
        state.set_selected_range(caret..caret, cx)
    });
    visual.run_until_parked();
    (input, visual)
}

/// A scrolled editor with the caret on the last line, in view.
fn scrolled_editor<'a>(
    cx: &'a mut TestAppContext,
    text: &str,
    provider: Arc<dyn ProjectionProvider>,
) -> (Entity<EditorState>, &'a mut VisualTestContext) {
    let (input, visual) = editor_at(cx, text, provider, text.len());
    let bounds = input.read_with(visual, |state, _| state.input_bounds());
    visual.simulate_event(ScrollWheelEvent {
        position: bounds.center(),
        delta: ScrollDelta::Pixels(point(px(0.), px(-100_000.))),
        ..Default::default()
    });
    visual.run_until_parked();
    (input, visual)
}

/// Enter on the last line with rows above that lose height while the edit
/// is laid out as source and regain it at adoption. Without an anchor on
/// that frame the rows above moved by their height change on every Enter.
#[gpui::test]
fn rows_changing_height_above_an_edit_never_move_the_view(cx: &mut TestAppContext) {
    let headings: String = (0..HEADINGS).map(|i| format!("Heading {i}\n")).collect();
    let body: String = (0..60).map(|i| format!("body line {i}\n")).collect();
    let text = format!("{headings}{body}");
    let provider = Arc::new(Headings {
        raw: AtomicBool::new(false),
    });
    let (input, visual) = scrolled_editor(cx, &text, provider.clone());
    let provider_ref = provider.clone() as Arc<dyn ProjectionProvider>;
    // The start of the last body line, right above the caret.
    let reference = text.len() - "body line 59\n".len();
    draw(&input, reference, visual);
    let mut before = draw(&input, reference, visual);
    input.read_with(visual, |state, _| {
        assert!(
            state.scroll_offset().y < px(-200.),
            "positive control: the editor is scrolled"
        )
    });
    assert!(before.caret.is_some() && before.reference.is_some());
    for key in 1..=10 {
        provider.raw.store(true, Ordering::SeqCst);
        visual.update(|window, cx| {
            window.dispatch_keystroke(Keystroke::parse("enter").unwrap(), cx);
        });
        let edit = draw(&input, reference, visual);
        input.read_with(visual, |state, _| {
            assert_eq!(
                state.value().len(),
                text.len() + key,
                "Enter {key} reached the editor"
            )
        });
        assert_held(&before, &edit, &format!("Enter {key}, drawn as source"));
        provider.raw.store(false, Ordering::SeqCst);
        input.update(visual, |state, cx| {
            state.set_projection_provider(Some(provider_ref.clone()), cx)
        });
        let adopted = draw(&input, reference, visual);
        assert_held(&edit, &adopted, &format!("Enter {key}, projection adopted"));
        before = adopted;
    }
}

// #1621 diagnostics: one frame record per laid-out keystroke.
/// Each laid-out keystroke leaves one frame record: the raw edit frame held by
/// the row it starts on, then the adoption held by the caret.
#[gpui::test]
fn layout_frames_name_the_raw_edit_frame_and_its_anchor(cx: &mut TestAppContext) {
    let headings: String = (0..HEADINGS).map(|i| format!("Heading {i}\n")).collect();
    let body: String = (0..60).map(|i| format!("body line {i}\n")).collect();
    let text = format!("{headings}{body}");
    let provider = Arc::new(Headings {
        raw: AtomicBool::new(false),
    });
    let (input, visual) = scrolled_editor(cx, &text, provider.clone());
    let provider_ref = provider.clone() as Arc<dyn ProjectionProvider>;
    input.update(visual, |state, _| state.enable_layout_diagnostics());
    draw(&input, 0, visual);
    input.read_with(visual, |state, _| state.take_layout_frames());
    provider.raw.store(true, Ordering::SeqCst);
    visual.update(|window, cx| {
        window.dispatch_keystroke(Keystroke::parse("enter").unwrap(), cx);
    });
    draw(&input, 0, visual);
    provider.raw.store(false, Ordering::SeqCst);
    input.update(visual, |state, cx| {
        state.set_projection_provider(Some(provider_ref), cx)
    });
    draw(&input, 0, visual);
    let frames = input.read_with(visual, |state, _| state.take_layout_frames());
    assert_eq!(frames.len(), 2, "{frames:?}");
    let (edit, adopted) = (&frames[0], &frames[1]);
    assert!(edit.edit && edit.raw && edit.anchor == "edit", "{edit:?}");
    // #1679 keeps the prepared Source wrap map: Enter wraps its two resulting
    // lines instead of the entire note. The raw-frame and scroll-anchor
    // assertions below must still hold while projection is unavailable.
    assert_eq!(edit.reshaped_lines, 2, "{edit:?}");
    // The headings lost half their height above the caret: the anchor
    // scrolled up by that, and caret-follow down by up to one row.
    let line = f32::from(input.read_with(visual, |state, _| state.cursor_layout().unwrap().1));
    let rows = edit.scroll_delta / line;
    assert!(
        [HEADINGS as f32 - 1., HEADINGS as f32]
            .iter()
            .any(|r| (rows - r).abs() < 0.05),
        "{edit:?} line {line}"
    );
    // #1304: the adoption anchors from the classified layout before the
    // raw frame, not from the raw rows.
    assert!(
        !adopted.edit && !adopted.raw && adopted.anchor == "reference",
        "{adopted:?}"
    );
    assert!(
        (adopted.scroll_delta + line * HEADINGS as f32).abs() < 0.5,
        "{adopted:?}"
    );
    assert!(input
        .read_with(visual, |state, _| state.take_layout_frames())
        .is_empty());
}

fn scroll_y(input: &Entity<EditorState>, visual: &mut VisualTestContext) -> f32 {
    f32::from(input.read_with(visual, |state, _| state.scroll_offset().y))
}

/// Draw the edit as source, then adopt the classification again, and check
/// that the view at the top of the note is where it was before the edit.
/// `edit` makes the change while the provider has no projection.
fn assert_top_held_across_a_raw_frame(
    cx: &mut TestAppContext,
    edit: impl FnOnce(&Entity<EditorState>, &mut VisualTestContext),
    setup: impl FnOnce(&Entity<EditorState>, &mut VisualTestContext),
    what: &str,
) {
    let headings: String = (0..HEADINGS).map(|i| format!("Heading {i}\n")).collect();
    let body: String = (0..60).map(|i| format!("body line {i}\n")).collect();
    let text = format!("{headings}{body}");
    let provider = Arc::new(Headings {
        raw: AtomicBool::new(false),
    });
    let heading = text.find("Heading 5").unwrap();
    let (input, visual) = editor_at(cx, &text, provider.clone(), heading + "Heading 5".len());
    let provider_ref = provider.clone() as Arc<dyn ProjectionProvider>;
    setup(&input, visual);
    draw(&input, heading, visual);
    let before = draw(&input, heading, visual);
    let top = before.reference.expect("the heading is in view");
    assert!(
        scroll_y(&input, visual).abs() < 0.5,
        "{what}: control, the note is at its top"
    );

    provider.raw.store(true, Ordering::SeqCst);
    edit(&input, visual);
    let raw = draw(&input, heading, visual);
    // Positive control: this frame is drawn raw, the five headings above lose
    // half their height, and the anchor's scroll down is clamped at the top.
    let line = raw.line;
    let raw_top = raw.reference.expect("the heading stays in view");
    let rows_up = f32::from(top - raw_top) / f32::from(line);
    assert!(
        (rows_up - 5.).abs() < 0.05,
        "{what}: control, the raw frame moved the heading {rows_up} rows"
    );
    assert!(
        scroll_y(&input, visual).abs() < 0.5,
        "{what}: control, clamped at the top"
    );

    provider.raw.store(false, Ordering::SeqCst);
    input.update(visual, |state, cx| {
        state.set_projection_provider(Some(provider_ref.clone()), cx)
    });
    for frame in ["adopted", "settled"] {
        let after = draw(&input, heading, visual);
        let scroll = scroll_y(&input, visual);
        assert!(
            scroll.abs() < 0.5,
            "{what}, {frame}: the note scrolled by {scroll} px (one row is {line:?})"
        );
        let moved = f32::from(after.reference.expect("the heading is in view") - top);
        assert!(
            moved.abs() < 0.5,
            "{what}, {frame}: the heading moved by {moved} px from before the edit"
        );
    }
}

/// #1304 (#1266): typing at the top of a note while the edit falls back to a
/// raw frame. The clamp at the top kept none of that frame's correction, and
/// the full reverse correction from the raw rows scrolled the note down by the
/// height the rows above lost.
#[gpui::test]
fn typing_at_the_top_through_a_raw_frame_keeps_the_view(cx: &mut TestAppContext) {
    assert_top_held_across_a_raw_frame(
        cx,
        |_, visual| visual.simulate_input("x"),
        |_, _| {},
        "typing",
    );
}

/// #1304 (#1266): Undo at the top of a note while the edit falls back to a
/// raw frame. Anchored from the raw layout, the next frame scrolled the note.
#[gpui::test]
fn undo_at_the_top_through_a_raw_frame_keeps_the_view(cx: &mut TestAppContext) {
    assert_top_held_across_a_raw_frame(
        cx,
        |input, visual| {
            let typed = input.read_with(visual, |state, _| state.value().len());
            visual.update(|window, cx| {
                window.dispatch_action(Box::new(gpui_component::input::Undo), cx)
            });
            input.read_with(visual, |state, _| {
                assert_eq!(state.value().len(), typed - 1, "Undo reached the editor")
            });
        },
        |input, visual| {
            visual.simulate_input("x");
            draw(input, 0, visual);
            input.read_with(visual, |state, _| {
                assert!(state.value().contains("Heading 5x"), "the typed character")
            });
        },
        "Undo",
    );
}
