use super::tests::{click, open, open_file, settle, value};
use super::*;
use ::core::prelude::v1::test;
use gpui_component::input::Undo;

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
    click(visual, "source-code-format");
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
fn large2804_plain_json_above_four_mib_formats_reopens_and_undoes(cx: &mut TestAppContext) {
    let source = format!(
        "{{\"values\":[\n{}\"tail\"]}}",
        format!("\"{}\",\n", "x".repeat(2048)).repeat(3000)
    );
    assert!(source.len() > 5 * 1024 * 1024);
    let (f, visual) = open_file(cx, &source, "large.JSON");
    f.reader.read_with(visual, |reader, cx| {
        assert!(
            reader.code_view_input(cx).is_none(),
            "large Source must not retain a hidden read preview"
        );
    });
    assert_eq!(
        f.input.read_with(visual, |input, _| input.value().len()),
        source.len()
    );
    click(visual, "source-code-validate");
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
    click(visual, "source-code-format");
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
        assert!(
            reader.code_file_editable(cx),
            "formatted large JSON can reopen in Edit"
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
    f.reader.read_with(visual, |reader, cx| {
        assert!(
            reader.code_view_input(cx).is_none(),
            "reopened large Source retires the rebuilt read preview"
        );
    });
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
    f.reader.update_in(visual, |reader, window, cx| {
        reader.code_operation(Action::Format, window, cx);
        assert!(reader.editing.as_ref().unwrap().operations.busy);
        assert!(cx
            .global::<JobGate>()
            .0
            .load(std::sync::atomic::Ordering::Acquire));
    });
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "button dispatch must not flatten/format a multi-MB target: {:?}",
        started.elapsed()
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
