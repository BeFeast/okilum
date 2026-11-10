use super::tests::{click, open, settle, value};
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
    visual.simulate_click(cancel.center(), Modifiers::default());
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
