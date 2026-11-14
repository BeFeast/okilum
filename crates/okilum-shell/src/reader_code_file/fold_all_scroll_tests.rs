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

/// Synthetic TOML of four tables of forty keys: 164 lines, and no fold over
/// the whole document, unlike JSON's root object.
fn issue2958_toml() -> String {
    (0..4)
        .map(|t| {
            let keys: String = (0..40)
                .map(|k| format!("key_{t}_{k} = {}\n", t * 100 + k))
                .collect();
            format!("[table_{t}]\n{keys}")
        })
        .collect()
}

#[gpui::test]
fn issue2958_fold_all_without_a_root_fold_paints_every_folded_table(cx: &mut TestAppContext) {
    let (preview, visual, _temp) = fixture(cx, "data.toml", &issue2958_toml());
    settled_frame(visual);
    let input = preview.read_with(visual, |p, _| {
        assert_eq!(p.view, View::Raw, "TOML opens Raw");
        p.input().expect("Raw view is the code editor")
    });
    let last_table = 3 * 41;
    let target = 2 * 41 + 10;
    input.read_with(visual, |s, _| {
        assert_eq!(s.value().lines().nth(last_table), Some("[table_3]"));
        assert_eq!(s.value().lines().nth(target), Some("key_2_9 = 209"));
    });
    scroll_to_line(&input, target, visual);

    let fold = visual
        .debug_bounds("code-fold-all")
        .expect("header Fold all button is mounted");
    visual.simulate_click(fold.center(), Modifiers::default());
    frame(visual);
    input.read_with(visual, |s, _| {
        let folds = s.folded_ranges();
        assert!(
            folds.iter().any(|fold| fold.start_line == 2 * 41),
            "Fold all folds the table above the old top line: {folds:?}"
        );
        assert!(
            !folds
                .iter()
                .any(|fold| fold.start_line == 0 && fold.end_line >= last_table),
            "positive control: no fold covers the whole document: {folds:?}"
        );
        let rows = s.visible_row_range();
        assert_eq!(
            s.top_visible_line(),
            Some(0),
            "the short folded text is shown from its first table in the first frame (rows {rows:?})"
        );
        assert!(painted_in_viewport(s, 0), "`[table_0]` is painted");
        assert!(
            painted_in_viewport(s, 2 * 41),
            "the old top line's table `[table_2]` is painted"
        );
        assert!(painted_in_viewport(s, last_table), "`[table_3]` is painted");
        assert!(
            s.scroll_offset().y <= px(0.) && s.scroll_offset().y >= px(-1.),
            "the scroll offset fits the short folded content: {:?}",
            s.scroll_offset()
        );
    });
}

#[test]
fn issue2958_batch_fold_top_keeps_the_scroll_range_filled() {
    use super::structure::{batch_fold_anchor, batch_fold_top};
    use gpui_component::input::FoldRange;
    // A 600 px viewport of 20 px lines: 30 rows, the bottom 15 may be empty.
    let top = |anchor, lines, folds: &[FoldRange]| {
        batch_fold_top(anchor, lines, folds, px(600.), px(20.))
    };
    // Unfolded: a top line in the last 15 lines moves up; others stay.
    assert_eq!(top(10, 100, &[]), 10);
    assert_eq!(top(85, 100, &[]), 85);
    assert_eq!(top(90, 100, &[]), 85);
    // Folded text shorter than the rows to fill: shown from its first line.
    let tables: Vec<FoldRange> = (0..4)
        .map(|t| FoldRange::new(t * 20, t * 20 + 20))
        .collect();
    assert_eq!(top(batch_fold_anchor(65, &tables), 81, &tables), 0);
    // Nested folds hide one merged range.
    let nested = [FoldRange::new(0, 99), FoldRange::new(10, 20)];
    assert_eq!(batch_fold_anchor(50, &nested), 0);
    assert_eq!(top(99, 100, &nested), 0);
    // A fold hides the lines between its header and closing line: 71..=80
    // and 95..=99 are the last 15 shown lines.
    let late = [FoldRange::new(80, 95)];
    assert_eq!(batch_fold_anchor(90, &late), 80);
    assert_eq!(top(80, 100, &late), 71);
    assert_eq!(top(60, 100, &late), 60);
    // A long fold far from the end changes nothing.
    let early = [FoldRange::new(10, 150)];
    assert_eq!(top(10, 200, &early), 10);
    assert_eq!(top(195, 200, &early), 185);
    // No layout yet: the anchor as is.
    assert_eq!(batch_fold_top(90, 100, &[], px(600.), px(0.)), 90);
}
