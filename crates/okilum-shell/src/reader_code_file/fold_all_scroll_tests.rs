//! #2958: Fold all / Unfold all while scrolled must leave a painted document.
use super::view_tests::fixture;
use super::*;
use ::core::prelude::v1::test;

/// Synthetic JSON that formats to about 141 lines: seven sections of
/// eighteen scalar fields under one root object.
fn issue2958_json() -> String {
    let sections: Vec<String> = (0..7)
        .map(|s| {
            let fields: Vec<String> = (0..18)
                .map(|f| format!("\"field_{s}_{f}\":{}", s * 100 + f))
                .collect();
            format!("\"section_{s}\":{{{}}}", fields.join(","))
        })
        .collect();
    format!("{{{}}}", sections.join(","))
}

/// One frame, as the platform draws it after an input event. Several draws
/// would hide the defect: the editor's own clamp after paint lands only in
/// a later frame, which the real app never requests.
fn frame(visual: &mut VisualTestContext) {
    visual.update(|window, cx| window.draw(cx).clear(cx));
}

fn settled_frame(visual: &mut VisualTestContext) {
    visual.run_until_parked();
    visual.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
}

/// The source byte range of `line` (0-based) in the editor text, without
/// its newline.
fn line_range(text: &str, line: usize) -> std::ops::Range<usize> {
    let start: usize = text.split_inclusive('\n').take(line).map(str::len).sum();
    let end = text[start..].find('\n').map_or(text.len(), |at| start + at);
    start..end
}

/// Whether `line` was painted inside the editor viewport by the last frame.
/// `range_to_bounds` maps an offset before the laid-out lines onto the first
/// of them, so the line must be laid out and not hidden inside a fold first.
fn painted_in_viewport(input: &EditorState, line: usize) -> bool {
    if !input
        .visible_row_range()
        .is_some_and(|lines| lines.contains(&line))
    {
        return false;
    }
    if input
        .folded_ranges()
        .iter()
        .any(|fold| fold.start_line < line && line < fold.end_line)
    {
        return false;
    }
    let text = input.value();
    let range = line_range(text.as_ref(), line);
    let Some(bounds) = input.range_to_bounds(&(range.start..range.start + 1)) else {
        return false;
    };
    let viewport = input.input_bounds();
    bounds.top() >= viewport.top() - px(1.) && bounds.bottom() <= viewport.bottom() + px(1.)
}

fn formatted_editor(
    preview: &Entity<CodePreview>,
    visual: &mut VisualTestContext,
) -> (Entity<EditorState>, usize) {
    settled_frame(visual);
    let input = preview.read_with(visual, |p, _| {
        assert_eq!(p.view, View::Formatted, "small JSON opens Formatted");
        p.input().expect("Formatted view is the code editor")
    });
    let lines = input.read_with(visual, |s, _| {
        assert!(s.folded_ranges().is_empty(), "opens unfolded");
        s.value().trim_end().lines().count()
    });
    assert!(
        (120..=160).contains(&lines),
        "fixture formats to about 141 lines, got {lines}"
    );
    (input, lines)
}

fn scroll_to_line(input: &Entity<EditorState>, line: usize, visual: &mut VisualTestContext) {
    input.update(visual, |s, cx| s.scroll_line_to_top(line, cx));
    settled_frame(visual);
    settled_frame(visual);
    input.read_with(visual, |s, _| {
        assert_eq!(
            s.top_visible_line(),
            Some(line),
            "positive control: line {line} is at the top before Fold all"
        );
        assert!(
            !painted_in_viewport(s, 0),
            "positive control: the root line is scrolled out of view"
        );
    });
}

fn assert_folded_document_painted(
    input: &Entity<EditorState>,
    lines: usize,
    visual: &mut VisualTestContext,
) {
    input.read_with(visual, |s, _| {
        assert!(
            s.folded_ranges().iter().any(|fold| fold.start_line == 0),
            "Fold all folds the root object"
        );
        let rows = s.visible_row_range();
        assert_eq!(
            s.top_visible_line(),
            Some(0),
            "the folded root is at the top of the first frame (rows {rows:?})"
        );
        assert!(
            painted_in_viewport(s, 0),
            "the root `{{ … ` row is painted inside the viewport"
        );
        assert!(
            painted_in_viewport(s, lines - 1),
            "the root's closing `}}` is painted inside the viewport"
        );
        assert!(
            s.scroll_offset().y <= px(0.) && s.scroll_offset().y >= px(-1.),
            "the scroll offset fits the short folded content: {:?}",
            s.scroll_offset()
        );
    });
}

#[gpui::test]
fn issue2958_header_fold_all_while_scrolled_paints_the_folded_root(cx: &mut TestAppContext) {
    let (preview, visual, _temp) = fixture(cx, "test1.json", &issue2958_json());
    let (input, lines) = formatted_editor(&preview, visual);
    scroll_to_line(&input, 25, visual);

    let fold = visual
        .debug_bounds("code-fold-all")
        .expect("header Fold all button is mounted");
    visual.simulate_click(fold.center(), Modifiers::default());
    frame(visual);
    assert_folded_document_painted(&input, lines, visual);

    // Unfold all round trip: the document comes back from its top, painted
    // in the first frame.
    let unfold = visual
        .debug_bounds("code-unfold-all")
        .expect("header Unfold all button is mounted");
    visual.simulate_click(unfold.center(), Modifiers::default());
    frame(visual);
    input.read_with(visual, |s, _| {
        assert!(s.folded_ranges().is_empty(), "Unfold all opens every fold");
        assert_eq!(s.top_visible_line(), Some(0));
        assert!(painted_in_viewport(s, 0));
        assert!(painted_in_viewport(s, 25), "the unfolded body is painted");
    });
}

#[gpui::test]
fn issue2958_fold_all_action_while_scrolled_paints_the_folded_root(cx: &mut TestAppContext) {
    let (preview, visual, _temp) = fixture(cx, "test1.json", &issue2958_json());
    let (input, lines) = formatted_editor(&preview, visual);
    scroll_to_line(&input, 19, visual);

    // The keyboard chord resolves to this action on every platform.
    visual.update(|window, cx| input.read(cx).focus_handle(cx).focus(window, cx));
    visual.dispatch_action(FoldAll);
    visual.run_until_parked();
    frame(visual);
    assert_folded_document_painted(&input, lines, visual);
}

#[gpui::test]
fn issue2958_unfold_all_keeps_the_top_source_line(cx: &mut TestAppContext) {
    let (preview, visual, _temp) = fixture(cx, "test1.json", &issue2958_json());
    let (input, _) = formatted_editor(&preview, visual);
    // A partly folded document: Fold all, then open the last section only.
    // Its second field is at the top while six folded sections sit above it.
    let target = input.read_with(visual, |s, _| {
        s.value()
            .lines()
            .position(|line| line.contains("\"field_6_1\""))
            .expect("fixture field")
    });
    preview.update(visual, |p, cx| p.fold_all(true, cx));
    input.update(visual, |s, cx| {
        use gpui_component::input::RopeExt as _;
        let at = line_range(s.value().as_ref(), target).start;
        assert!(s.unfold_at(s.text().offset_to_position(at), cx));
    });
    settled_frame(visual);
    scroll_to_line(&input, target, visual);

    let unfold = visual
        .debug_bounds("code-unfold-all")
        .expect("header Unfold all button is mounted");
    visual.simulate_click(unfold.center(), Modifiers::default());
    frame(visual);
    input.read_with(visual, |s, _| {
        assert!(s.folded_ranges().is_empty(), "Unfold all opens every fold");
        assert_eq!(
            s.top_visible_line(),
            Some(target),
            "Unfold all keeps the source line that was at the top"
        );
        assert!(painted_in_viewport(s, target));
    });
}

#[gpui::test]
fn issue2958_tree_fold_all_while_scrolled_paints_the_first_rows(cx: &mut TestAppContext) {
    let (preview, visual, _temp) = fixture(cx, "test1.json", &issue2958_json());
    preview.update_in(visual, |p, window, cx| {
        p.set_view(View::Tree, window, cx);
        p.fold_all(false, cx);
    });
    settled_frame(visual);
    let target = preview.read_with(visual, |p, _| {
        assert!(p.structure.visible.len() > 100, "unfolded Tree rows");
        p.structure
            .scroll
            .scroll_to_item_strict(60, ScrollStrategy::Top);
        p.structure.visible[60]
    });
    settled_frame(visual);
    settled_frame(visual);
    let viewport = visual
        .debug_bounds("data-structure-viewport")
        .expect("Tree viewport is mounted");
    let row = visual
        .debug_bounds(Box::leak(
            format!("data-structure-row-{target}").into_boxed_str(),
        ))
        .expect("positive control: the scrolled-to row is painted");
    assert!(row.top() < viewport.top() + px(40.), "row 60 is at the top");

    let fold = visual
        .debug_bounds("code-fold-all")
        .expect("header Fold all button is mounted");
    visual.simulate_click(fold.center(), Modifiers::default());
    frame(visual);
    let first = preview.read_with(visual, |p, _| {
        assert_eq!(
            p.structure.visible.len(),
            7,
            "one row per top-level section"
        );
        p.structure.visible[0]
    });
    let row = visual
        .debug_bounds(Box::leak(
            format!("data-structure-row-{first}").into_boxed_str(),
        ))
        .expect("the first folded Tree row is painted");
    assert!(
        row.top() >= viewport.top() - px(1.) && row.top() < viewport.top() + px(8.),
        "the first folded row is at the top of the Tree viewport: {row:?} in {viewport:?}"
    );
}
