use super::tests::{click, open, open_file, settle, value, Fixture};
use super::*;
use ::core::prelude::v1::test;
use gpui_component::input::{Redo, Undo};

fn document(valid: bool) -> String {
    // Same scale as the tester's OpenAPI, with spaces and Unicode retained.
    format!(
        "Before\n```json\n{{{}: \"{}\",\n\"unicode\": \"שלום \\u0041\"}}\n```\nAfter\n",
        if valid { "\"payload\"" } else { "payload" },
        "value ".repeat(600_000)
    )
}

#[gpui::test]
fn large2804_plain_file_open_retires_markdown_outline(cx: &mut TestAppContext) {
    let (f, visual) = open(cx, "# A real heading\n\n{note}\n");
    f.reader.read_with(visual, |reader, _| {
        assert!(reader
            .visible_outline()
            .iter()
            .any(|h| h.text == "A real heading"));
    });
    let text = "# Literal plain-file data\n{value}\n";
    std::fs::write(f.directory.path().join("vault/data.txt"), text).unwrap();
    f.reader.update_in(visual, |reader, window, cx| {
        reader.toggle_source(window, cx);
        reader.preview_file("data.txt", window, cx);
    });
    visual.run_until_parked();
    f.reader.update_in(visual, |reader, window, cx| {
        reader.toggle_source(window, cx)
    });
    visual.run_until_parked();
    f.reader.read_with(visual, |reader, cx| {
        assert_eq!(reader.selected_file(), "data.txt");
        assert_eq!(
            reader
                .editing
                .as_ref()
                .unwrap()
                .input
                .read(cx)
                .value()
                .as_ref(),
            text
        );
        assert!(
            reader.visible_outline().is_empty(),
            "plain data must not be parsed as Markdown or inherit the old note's headings"
        );
    });
}

#[gpui::test]
fn large2804_plain_file_mutation_never_parses_markdown_outline(cx: &mut TestAppContext) {
    let original = "Plain data\n{value}\n";
    let (f, visual) = open_file(cx, original, "data.txt");
    f.reader.read_with(visual, |reader, _| {
        assert!(reader.visible_outline().is_empty())
    });
    f.input.update_in(visual, |input, window, cx| {
        input.replace_source_range(0..0, "# Still plain data\n", window, cx);
    });
    visual.run_until_parked();
    f.reader.read_with(visual, |reader, cx| {
        assert_eq!(value(&f, cx), format!("# Still plain data\n{original}"));
        assert!(
            reader.visible_outline().is_empty(),
            "a plain-file mutation cannot launch the Markdown parser"
        );
    });
}

#[gpui::test]
fn large2804_plain_json_format_growth_retires_small_read_preview(cx: &mut TestAppContext) {
    let source = format!("{{\"rows\":[{}0]}}", "0,".repeat(600_000));
    assert!(source.len() < 4 * 1024 * 1024);
    let (f, visual) = open_file(cx, &source, "growth.json");
    f.reader.read_with(visual, |reader, cx| {
        assert!(
            reader.code_view_input(cx).is_some(),
            "small read preview positive control"
        );
    });
    click(visual, "file-format");
    f.reader.read_with(visual, |reader, cx| {
        assert!(value(&f, cx).len() > 4 * 1024 * 1024);
        assert!(
            reader.code_view_input(cx).is_none(),
            "growing Format result must release the hidden small preview too"
        );
    });
    visual.dispatch_action(Undo);
    settle(visual);
    f.reader
        .read_with(visual, |_, cx| assert_eq!(value(&f, cx), source));
}

#[gpui::test]
fn large2804_grown_plain_json_formats_undoes_and_saves_readonly(cx: &mut TestAppContext) {
    let source = format!(
        "{{\"values\":[\n{}\"tail\"]}}",
        format!("\"{}\",\n", "x".repeat(2048)).repeat(3000)
    );
    assert!(source.len() > 5 * 1024 * 1024);
    // Admission now has a 4 MiB UI cap (#2809). An existing editing session
    // can still grow beyond it without losing the full source-tool budget.
    let seed = "{\"seed\":0}";
    let (f, visual) = open_file(cx, seed, "large.JSON");
    f.input.update_in(visual, |input, window, cx| {
        input.replace_source_range(0..seed.len(), &source, window, cx);
    });
    settle(visual);
    assert_eq!(
        f.input.read_with(visual, |input, _| input.value().len()),
        source.len()
    );
    file_validate(visual);
    f.reader.read_with(visual, |reader, _| {
        assert_eq!(
            reader
                .editing
                .as_ref()
                .unwrap()
                .operations
                .status
                .as_ref()
                .unwrap()
                .3,
            "Valid strict JSON"
        );
    });
    click(visual, "file-format");
    let formatted = f
        .input
        .read_with(visual, |input, _| input.value().to_string());
    assert_ne!(formatted, source);
    f.reader.read_with(visual, |reader, cx| {
        assert!(
            reader.code_view_input(cx).is_none(),
            "formatting the grown large Source retires the hidden small preview"
        );
    });
    visual.dispatch_action(Undo);
    settle(visual);
    f.reader
        .read_with(visual, |_, cx| assert_eq!(value(&f, cx), source));
    click(visual, "file-format");
    f.reader
        .read_with(visual, |_, cx| assert_eq!(value(&f, cx), formatted));
    f.reader.update_in(visual, |reader, window, cx| {
        reader.toggle_source(window, cx)
    });
    visual.run_until_parked();
    f.reader.read_with(visual, |reader, cx| {
        assert!(reader.editing.is_none());
        assert!(
            !reader.code_file_editable(cx),
            "the saved file above 4 MiB is view-only under the new UI cap"
        );
        assert!(
            !reader.code_view_input(cx).unwrap().read(cx).is_editable(),
            "the viewer itself stays read-only"
        );
    });
    assert_eq!(
        std::fs::read_to_string(f.directory.path().join("vault/large.JSON")).unwrap(),
        formatted
    );
    f.reader.update_in(visual, |reader, window, cx| {
        reader.toggle_source(window, cx)
    });
    visual.run_until_parked();
    f.reader.read_with(visual, |reader, _| {
        assert!(
            reader.editing.is_none(),
            "the keyboard cannot bypass the cap"
        );
    });
    assert_eq!(
        std::fs::read_to_string(f.directory.path().join("vault/large.JSON")).unwrap(),
        formatted,
        "refused re-entry preserves the saved source"
    );
}

#[gpui::test]
fn large2804_plain_json_below_cap_reopens_and_undoes_format(cx: &mut TestAppContext) {
    let source = format!(
        "{{\"values\":[\n{}\"tail\"]}}",
        format!("\"{}\",\n", "x".repeat(2048)).repeat(1000)
    );
    assert!(source.len() > 1024 * 1024 && source.len() < 4 * 1024 * 1024);
    let (f, visual) = open_file(cx, &source, "reopen.JSON");
    click(visual, "file-format");
    let formatted = f
        .input
        .read_with(visual, |input, _| input.value().to_string());
    assert_ne!(formatted, source);
    f.reader.update_in(visual, |reader, window, cx| {
        reader.toggle_source(window, cx)
    });
    visual.run_until_parked();
    f.reader.read_with(visual, |reader, cx| {
        assert!(reader.editing.is_none());
        assert!(reader.code_file_editable(cx), "below-cap re-entry positive");
        assert!(!reader.code_view_input(cx).unwrap().read(cx).is_editable());
    });
    assert_eq!(
        std::fs::read_to_string(f.directory.path().join("vault/reopen.JSON")).unwrap(),
        formatted
    );
    f.reader.update_in(visual, |reader, window, cx| {
        reader.toggle_source(window, cx)
    });
    visual.run_until_parked();
    visual.dispatch_action(Undo);
    settle(visual);
    f.reader.read_with(visual, |reader, cx| {
        assert_eq!(
            reader
                .editing
                .as_ref()
                .unwrap()
                .input
                .read(cx)
                .value()
                .as_ref(),
            source
        );
    });
}

#[gpui::test]
fn large2804_worker_cancel_keeps_source_and_ui_responsive(cx: &mut TestAppContext) {
    let original = document(true);
    let (f, visual) = open(cx, &original);
    let started = Instant::now();
    // update_in flushes GPUI effects after its callback. Keep that enclosing
    // update separate from the handler's preparation budget; real paint and
    // Cancel are still checked below.
    let mut handler_elapsed = None;
    f.reader.update_in(visual, |reader, window, cx| {
        let handler_started = Instant::now();
        reader.code_operation(Action::Format, window, cx);
        handler_elapsed = Some(handler_started.elapsed());
        assert!(reader.editing.as_ref().unwrap().operations.busy);
        assert!(cx
            .global::<JobGate>()
            .0
            .load(std::sync::atomic::Ordering::Acquire));
    });
    let handler_elapsed = handler_elapsed.expect("operation handler was dispatched");
    diagnostic_error!(
        "LARGE2804_DISPATCH handler={:?} enclosing_update={:?}",
        handler_elapsed,
        started.elapsed()
    );
    assert!(
        handler_elapsed < Duration::from_millis(100),
        "button handler must not flatten/format a multi-MB target: {:?}",
        handler_elapsed
    );
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        visual
            .debug_bounds("source-code-operation-cancel")
            .is_some(),
        "visible Cancel before worker completion"
    );
    assert!(f.input.read_with(visual, |input, _| input.is_editable()));
    let cancel = visual
        .debug_bounds("source-code-operation-cancel")
        .expect("visible Cancel");
    // simulate_click drains *all* queued worker work after mouse-down, before
    // mouse-up invokes Button::on_click. Deliver both actual pointer events in
    // one UI turn so this is a click while the operation is still pending.
    visual.update(|window, cx| {
        use gpui::InputEvent;
        window.dispatch_event(
            gpui::MouseDownEvent {
                position: cancel.center(),
                modifiers: Modifiers::default(),
                button: gpui::MouseButton::Left,
                click_count: 1,
                first_mouse: false,
            }
            .to_platform_input(),
            cx,
        );
        window.dispatch_event(
            gpui::MouseUpEvent {
                position: cancel.center(),
                modifiers: Modifiers::default(),
                button: gpui::MouseButton::Left,
                click_count: 1,
            }
            .to_platform_input(),
            cx,
        );
    });
    f.reader.read_with(visual, |reader, _| {
        let state = &reader.editing.as_ref().unwrap().operations;
        assert_eq!(state.notice.as_deref(), Some("Cancelling…"));
        assert!(state.running.as_ref().unwrap().control.cancelled());
    });
    visual.run_until_parked();
    f.reader.read_with(visual, |reader, cx| {
        let state = &reader.editing.as_ref().unwrap().operations;
        assert!(!state.busy && state.preview.is_none());
        assert_eq!(state.notice.as_deref(), Some("Operation cancelled."));
        assert!(!cx
            .global::<JobGate>()
            .0
            .load(std::sync::atomic::Ordering::Acquire));
        assert_eq!(value(&f, cx), original);
    });
    assert_eq!(
        std::fs::read_to_string(f.directory.path().join("vault/note.md")).unwrap(),
        original
    );
    // Positive control: cancellation retired a real formatter, not a refused job.
    click(visual, "source-code-format");
    f.reader
        .read_with(visual, |_, cx| assert_ne!(value(&f, cx), original));
    visual.dispatch_action(Undo);
    settle(visual);
    f.reader
        .read_with(visual, |_, cx| assert_eq!(value(&f, cx), original));
}

#[gpui::test]
fn large2804_repair_preview_is_bounded_apply_and_format_are_single_undo(cx: &mut TestAppContext) {
    let original = document(false);
    let (f, visual) = open(cx, &original);
    click(visual, "source-code-repair");
    f.reader.read_with(visual, |reader, cx| {
        let preview = reader
            .editing
            .as_ref()
            .unwrap()
            .operations
            .preview
            .as_ref()
            .expect("multi-MB repair available");
        assert!(preview.target.text.len() > 3 * 1024 * 1024);
        assert!(preview.proposed.len() > 3 * 1024 * 1024);
        assert!(preview.before.read(cx).value().len() <= PREVIEW_BYTES);
        assert!(preview.after.read(cx).value().len() <= PREVIEW_BYTES);
        assert!(preview.explanation.contains("first 64 KiB"));
        assert_eq!(preview.validation, "Valid strict JSON");
        assert_eq!(value(&f, cx), original);
    });
    assert_eq!(
        std::fs::read_to_string(f.directory.path().join("vault/note.md")).unwrap(),
        original
    );
    click(visual, "source-code-repair-cancel");
    f.reader
        .read_with(visual, |_, cx| assert_eq!(value(&f, cx), original));
    click(visual, "source-code-repair");
    click(visual, "source-code-repair-apply");
    f.reader.read_with(visual, |_, cx| {
        let text = value(&f, cx);
        assert_eq!(text, original.replacen("{payload:", "{\"payload\":", 1));
        assert!(text.ends_with("}\n```\nAfter\n"));
        assert!(text.contains("שלום \\u0041"));
    });
    visual.dispatch_action(Undo);
    settle(visual);
    f.reader
        .read_with(visual, |_, cx| assert_eq!(value(&f, cx), original));
}

#[gpui::test]
fn large2804_stale_completion_never_replaces_newer_large_source(cx: &mut TestAppContext) {
    let original = document(true);
    let (f, visual) = open(cx, &original);
    f.reader.update_in(visual, |reader, window, cx| {
        reader.code_operation(Action::Format, window, cx)
    });
    f.input.update_in(visual, |input, window, cx| {
        input.replace_source_range(0..6, "Newer!", window, cx);
    });
    visual.run_until_parked();
    f.reader.read_with(visual, |reader, cx| {
        assert_eq!(value(&f, cx), original.replacen("Before", "Newer!", 1));
        let state = &reader.editing.as_ref().unwrap().operations;
        assert!(!state.busy);
        assert!(state.notice.as_deref().unwrap().contains("Source changed"));
    });
}

#[gpui::test]
fn large2804_resource_refusal_has_one_message_not_invalid(cx: &mut TestAppContext) {
    let original = format!(
        "```json\n{{\"deep\":{}0{}}}\n```\n",
        "[".repeat(129),
        "]".repeat(129)
    );
    let (f, visual) = open(cx, &original);
    click(visual, "source-code-validate");
    click(visual, "source-code-format");
    f.reader.read_with(visual, |reader, cx| {
        let state = &reader.editing.as_ref().unwrap().operations;
        assert!(
            state.status.is_none(),
            "no separate validation refusal line"
        );
        let message = state.notice.as_ref().unwrap();
        assert!(message.contains("128 levels") && message.contains("bytes"));
        assert!(!message.to_lowercase().contains("invalid"));
        assert_eq!(value(&f, cx), original);
    });
}

#[test]
fn large2804_preview_clips_on_utf8_boundary_without_changing_proposal() {
    let source = format!("{}שלום", "x".repeat(PREVIEW_BYTES - 1));
    assert_eq!(preview_excerpt(&source).len(), PREVIEW_BYTES - 1);
    assert!(source.starts_with(preview_excerpt(&source)));
    assert!(source.ends_with("שלום"));
}

#[gpui::test]
fn large2804_completion_retains_composition_started_after_request(cx: &mut TestAppContext) {
    use gpui::EntityInputHandler;
    let original = "```json\n{\"a\":1}\n```\n";
    let (f, visual) = open(cx, original);
    f.reader.update_in(visual, |reader, window, cx| {
        reader.code_operation(Action::Format, window, cx)
    });
    f.input.update_in(visual, |input, window, cx| {
        input.replace_and_mark_text_in_range(None, "か", None, window, cx)
    });
    let composing = f
        .input
        .read_with(visual, |input, _| input.value().to_string());
    visual.run_until_parked();
    f.reader.read_with(visual, |reader, cx| {
        assert_eq!(value(&f, cx), composing);
        assert!(!reader.editing.as_ref().unwrap().operations.busy);
    });
    f.input.update_in(visual, |input, window, cx| {
        assert!(input.marked_text_range(window, cx).is_some());
        input.unmark_text(window, cx);
    });
    visual.run_until_parked();
}

#[gpui::test]
fn large2804_cancel_stays_visible_when_caret_leaves_requested_fence(cx: &mut TestAppContext) {
    let original = "Before\n```json\n{\"a\":1}\n```\nAfter\n";
    let (f, visual) = open(cx, original);
    f.reader.update_in(visual, |reader, window, cx| {
        reader.code_operation(Action::Format, window, cx)
    });
    f.input
        .update(visual, |input, cx| input.set_selected_range(0..0, cx));
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual
        .debug_bounds("source-code-operation-cancel")
        .is_some());
    f.reader
        .update(visual, |reader, cx| reader.cancel_code_operation(cx));
    visual.run_until_parked();
    f.reader
        .read_with(visual, |_, cx| assert_eq!(value(&f, cx), original));
}

#[gpui::test]
fn large2804_rebound_reader_rejects_result_even_with_identical_native_stamp(
    cx: &mut TestAppContext,
) {
    let original = "Before\n```json\n{\"a\":1}\n```\nAfter\n";
    let (f, visual) = open(cx, original);
    let before = f.input.read_with(visual, |input, _| input.source_stamp());
    std::fs::write(f.directory.path().join("vault/other.md"), original).unwrap();
    f.reader.update_in(visual, |reader, window, cx| {
        reader.code_operation(Action::Format, window, cx);
        reader.current_rel = "other.md".into();
        assert_eq!(reader.selected_file(), "other.md");
    });
    assert_eq!(
        f.input.read_with(visual, |input, _| input.source_stamp()),
        before,
        "positive control: a stamp-only completion guard would accept this result"
    );
    visual.run_until_parked();
    f.reader.update(visual, |reader, cx| {
        assert_eq!(value(&f, cx), original);
        assert!(reader
            .editing
            .as_ref()
            .unwrap()
            .operations
            .notice
            .as_deref()
            .unwrap()
            .contains("Source changed"));
        reader.current_rel = "note.md".into();
    });
    assert_eq!(
        std::fs::read_to_string(f.directory.path().join("vault/other.md")).unwrap(),
        original
    );
}

#[gpui::test]
fn large2804_repair_apply_rejects_rebound_binding_with_same_source(cx: &mut TestAppContext) {
    let original = "Before\n```json\n{a:1}\n```\nAfter\n";
    let (f, visual) = open(cx, original);
    click(visual, "source-code-repair");
    std::fs::write(f.directory.path().join("vault/other.md"), original).unwrap();
    f.reader.update_in(visual, |reader, window, cx| {
        assert!(reader
            .editing
            .as_ref()
            .unwrap()
            .operations
            .preview
            .is_some());
        reader.current_rel = "other.md".into();
        assert_eq!(reader.selected_file(), "other.md");
        reader.apply_code_repair(window, cx);
        assert_eq!(value(&f, cx), original);
        assert!(reader
            .editing
            .as_ref()
            .unwrap()
            .operations
            .notice
            .as_deref()
            .unwrap()
            .contains("stale"));
        reader.current_rel = "note.md".into();
    });
}

// File-only selector migration: the same real Validate command now lives in
// the split menu. Fenced-note selectors and all data/revision assertions stay.
fn file_validate(visual: &mut VisualTestContext) {
    let bounds = crate::reader_viewer_chrome::tests::measured_bounds(visual, "file-format-split")
        .expect("file Format split mounted");
    visual.simulate_click(
        point(bounds.right() - px(10.), bounds.center().y),
        Modifiers::default(),
    );
    visual.simulate_keystrokes("down down enter");
    settle(visual);
}

/// Formatter output for `rows` objects, so a fixture differs from Format's
/// result only where a test changes it.
fn formatted_rows(rows: usize) -> String {
    let minified = format!(
        "[{}]",
        (0..rows)
            .map(|i| format!("{{\"id\":{i},\"name\":\"item {i} שלום\",\"tags\":[\"a\",\"b\"]}}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    match model::format_indented(
        "json",
        &minified,
        model::LineEnding::Lf,
        &indent_unit(),
        &model::Control::default(),
    ) {
        Some(model::Formatted::Changed(text)) => text,
        _ => panic!("fixture formats"),
    }
}

/// Lines the editor re-wrapped for source edits since the last call.
fn rewrapped_lines(f: &Fixture, visual: &mut VisualTestContext) -> usize {
    visual.update(|window, cx| window.draw(cx).clear(cx));
    f.input.update(visual, |input, _| {
        input
            .take_layout_frames()
            .iter()
            .filter(|frame| frame.edit)
            .map(|frame| frame.reshaped_lines)
            .sum()
    })
}

/// #2804: applying a multi-MB Format/Repair result, and undoing it, re-wraps
/// only the lines that change, whatever the file size. A whole-file
/// replacement re-wrapped (and stored in Undo) every line, twice.
#[gpui::test]
fn large2804_apply_and_undo_touch_only_changed_lines(cx: &mut TestAppContext) {
    let pretty = formatted_rows(30_000);
    let lines = pretty.lines().count();
    assert!(pretty.len() > 2 * 1024 * 1024 && lines > 200_000);
    // Repair: three bare keys at the start, middle and end of the file.
    let mut broken = pretty.clone();
    for i in [2, 15_000, 29_998] {
        let quoted = format!("\"id\": {i},");
        let at = broken.find(&quoted).unwrap();
        broken.replace_range(at..at + quoted.len(), &format!("id: {i},"));
    }
    let (f, visual) = open_file(cx, &broken, "data.json");
    f.input
        .update(visual, |input, _| input.enable_layout_diagnostics());
    rewrapped_lines(&f, visual);
    f.reader.update_in(visual, |reader, window, cx| {
        reader.code_operation(Action::Repair, window, cx)
    });
    visual.run_until_parked();
    f.reader.update_in(visual, |reader, window, cx| {
        assert_eq!(
            reader
                .editing
                .as_ref()
                .unwrap()
                .operations
                .preview
                .as_ref()
                .unwrap()
                .edits
                .len(),
            3
        );
        reader.apply_code_repair(window, cx)
    });
    f.reader.read_with(visual, |_, cx| {
        assert_eq!(value(&f, cx), pretty, "exact Repair result")
    });
    let applied = rewrapped_lines(&f, visual);
    visual.dispatch_action(Undo);
    f.reader.read_with(visual, |_, cx| {
        assert_eq!(value(&f, cx), broken, "one Undo restores every byte")
    });
    let undone = rewrapped_lines(&f, visual);
    assert!(
        applied <= 16 && undone <= 16,
        "Repair re-wrapped {applied} and Undo {undone} of {lines} lines"
    );
    visual.dispatch_action(Redo);
    f.reader
        .read_with(visual, |_, cx| assert_eq!(value(&f, cx), pretty));
    visual.dispatch_action(Undo);
    settle(visual);

    // Format: two objects collapsed onto one line each.
    let mut collapsed = pretty.clone();
    for i in [7, 22_222] {
        let start = collapsed.find(&format!("\"id\": {i},")).unwrap();
        let start = collapsed[..start].rfind('{').unwrap();
        let end = start + collapsed[start..].find('}').unwrap();
        let flat: String = collapsed[start..end].lines().map(str::trim_start).collect();
        collapsed.replace_range(start..end, &flat);
    }
    f.input.update_in(visual, |input, window, cx| {
        let len = input.text().len();
        input.replace_source_range(0..len, &collapsed, window, cx);
    });
    settle(visual);
    rewrapped_lines(&f, visual);
    let source = value_of(&f, visual);
    f.reader.update_in(visual, |reader, window, cx| {
        reader.code_operation(Action::Format, window, cx)
    });
    visual.run_until_parked();
    f.reader.read_with(visual, |_, cx| {
        assert_eq!(value(&f, cx), pretty, "exact Format result")
    });
    let applied = rewrapped_lines(&f, visual);
    visual.dispatch_action(Undo);
    f.reader.read_with(visual, |_, cx| {
        assert_eq!(value(&f, cx), source, "one Undo restores every byte")
    });
    let undone = rewrapped_lines(&f, visual);
    assert!(
        applied <= 64 && undone <= 64,
        "Format re-wrapped {applied} and Undo {undone} of {lines} lines"
    );

    // Positive control: the same counter sees a whole-file change.
    f.input.update_in(visual, |input, window, cx| {
        let len = input.text().len();
        input.replace_source_range(0..len, &format!(" {source}"), window, cx);
    });
    let whole = rewrapped_lines(&f, visual);
    let lines = source.lines().count();
    assert!(
        whole >= lines,
        "control: a whole-file replacement re-wraps {whole} of {lines} lines"
    );
}

fn value_of(f: &Fixture, visual: &mut VisualTestContext) -> String {
    f.input
        .read_with(visual, |input, _| input.value().to_string())
}

/// The allocator observes native preflight and selection clipping, including
/// the old full-source String copies. Rejected groups must remain atomic.
#[gpui::test]
fn large2804_grouped_edit_boundaries_do_not_copy_the_document(cx: &mut TestAppContext) {
    use crate::reader_editor::work;

    let rows = "row\r\n".repeat(180_000);
    // The shared file fixture places its initial caret at the first brace.
    let original = format!("{rows}שלום🙂{rows}{{tail");
    let marker = rows.len();
    let (f, visual) = open_file(cx, &original, "data.txt");
    settle(visual);
    let stamp = f.input.read_with(visual, |input, _| input.source_stamp());
    // A valid first edit followed by an invalid UTF-8 boundary must not
    // partially apply. The remaining cases cover EOF and selection bounds.
    let invalid = [
        (
            vec![(0..3, "ROW".into()), (marker + 1..marker + 2, "x".into())],
            0..0,
        ),
        (vec![(original.len()..original.len() + 1, "x".into())], 0..0),
        (
            vec![(0..3, "ROW".into())],
            original.len() + 1..original.len() + 1,
        ),
        (Vec::new(), 0..0),
    ];
    for (edits, selection) in invalid {
        work::reset();
        f.input.update_in(visual, |input, window, cx| {
            let _scope = work::scope();
            assert!(!input.replace_source_edits(&edits, selection, window, cx));
        });
        let measured = work::take();
        assert!(
            measured.largest < 128 * 1024,
            "preflight copied the source: {measured:?}"
        );
        f.input
            .read_with(visual, |input, _| assert_eq!(input.source_stamp(), stamp));
        assert_eq!(value_of(&f, visual), original);
    }

    let edits = vec![
        (0..3, "ROW".into()),
        (marker..marker + "שלום🙂".len(), "א🙂ל".into()),
        (original.len() - 4..original.len(), "END".into()),
    ];
    work::reset();
    f.input.update_in(visual, |input, window, cx| {
        let _scope = work::scope();
        // Start inside the inserted emoji: clip left, as before.
        assert!(input.replace_source_edits(&edits, marker + 3..marker + 6, window, cx));
    });
    let measured = work::take();
    assert!(
        measured.largest < 128 * 1024,
        "grouped apply copied the source: {measured:?}"
    );
    let expected = format!("ROW{}א🙂ל{rows}{{END", &rows[3..]);
    assert_eq!(value_of(&f, visual), expected);
    f.input.read_with(visual, |input, _| {
        assert_eq!(input.selected_range(), marker + 2..marker + 6);
    });
    visual.dispatch_action(Undo);
    assert_eq!(
        value_of(&f, visual),
        original,
        "one Undo restores all three edits"
    );
    visual.dispatch_action(Redo);
    assert_eq!(
        value_of(&f, visual),
        expected,
        "one Redo restores the group"
    );

    // Positive control: the same thread-local allocator must see an actual
    // full-source conversion; a disabled or unfired meter cannot pass.
    work::reset();
    let flat = f.input.read_with(visual, |input, _| {
        let _scope = work::scope();
        input.text().to_string()
    });
    let control = work::take();
    assert_eq!(flat, expected);
    assert!(
        control.largest >= expected.len(),
        "unfired allocation control: {control:?}"
    );
}

/// Bulk row indexing feeds the tree without a contiguous LineItem buffer.
/// The source/history strings are deliberately much smaller than that buffer.
#[gpui::test]
fn large2804_bulk_rows_do_not_allocate_a_contiguous_row_buffer(cx: &mut TestAppContext) {
    use crate::reader_editor::work;

    let original = format!("{{{}tail", "a\r\n".repeat(160_000));
    let replacement = format!("{{{}end🙂", "ב\n".repeat(180_000));
    let (f, visual) = open_file(cx, &original, "data.txt");
    settle(visual);
    // Both source strings are below 1 MiB. The former 180k-row Vec is
    // several MiB; tree leaf/node allocations stay well below this bound.
    const ALLOCATION_LIMIT: usize = 2 * 1024 * 1024;
    assert!(original.len() < ALLOCATION_LIMIT / 2);
    assert!(replacement.len() < ALLOCATION_LIMIT / 2);

    work::reset();
    f.input.update_in(visual, |input, window, cx| {
        let _scope = work::scope();
        input.replace_source_range(0..original.len(), &replacement, window, cx);
    });
    let applied = work::take();
    assert!(
        applied.largest < ALLOCATION_LIMIT,
        "bulk apply allocated a contiguous row buffer: {applied:?}"
    );
    assert_eq!(value_of(&f, visual), replacement);
    settle(visual);
    work::reset();
    {
        let _scope = work::scope();
        visual.dispatch_action(Undo);
    }
    let undone = work::take();
    assert!(
        undone.largest < ALLOCATION_LIMIT,
        "bulk Undo allocated a contiguous row buffer: {undone:?}"
    );
    assert_eq!(value_of(&f, visual), original, "one byte-exact Undo");
    visual.dispatch_action(Redo);
    assert_eq!(value_of(&f, visual), replacement, "one byte-exact Redo");

    // An actual contiguous allocation must be visible to the same meter.
    work::reset();
    let control = {
        let _scope = work::scope();
        std::hint::black_box(vec![0u8; ALLOCATION_LIMIT * 2])
    };
    let measured = work::take();
    assert!(
        measured.largest >= control.len(),
        "unfired row-buffer allocation control: {measured:?}"
    );
}
