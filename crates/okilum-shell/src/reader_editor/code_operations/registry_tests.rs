use super::tests::{open_file, Fixture};
use super::*;
use ::core::prelude::v1::test;
use gpui_component::input::{Redo, Undo};

#[gpui::test]
fn edit2804_read_releases_editor_before_focus_lookup_and_preserves_history(
    cx: &mut TestAppContext,
) {
    let original = format!(
        "<html>\r\n<script>function value() {{ return 1; }}</script>\r\n{}\r\n</html>\r\n",
        "<p>שלום🙂 e\u{301} keeps exact source bytes</p>\r\n".repeat(7_000)
    );
    assert!(original.len() > 256 * 1024);
    let (f, visual) = open_file(cx, &original, "registry.html");
    visual.executor().advance_clock(Duration::from_millis(200));
    visual.run_until_parked();
    let prefix = "<!-- local שלום🙂 -->\r\n";
    f.input.update_in(visual, |input, window, cx| {
        assert!(input.replace_source_edits(&[(0..0, prefix.into())], 0..0, window, cx));
        let at = prefix.find("שלום").unwrap();
        input.set_selected_range(at..at + "שלום".len(), cx);
    });
    visual.run_until_parked();
    visual.executor().advance_clock(Duration::from_millis(2100));
    visual.run_until_parked();
    let prepared = format!("{prefix}{original}");
    assert_eq!(
        f.input.read_with(visual, |i, _| i.value().to_string()),
        prepared
    );
    assert!(!f.input.read_with(visual, |i, _| i.highlighting_pending()));
    let selected = f.input.read_with(visual, |i, _| i.selected_range());
    let stamp = f.input.read_with(visual, |i, _| i.source_stamp());
    let released = Rc::new(std::cell::Cell::new(0));
    let seen = released.clone();
    let _release = f.input.update(visual, |_, cx| {
        cx.on_release(move |_, _| seen.set(seen.get() + 1))
    });
    let Fixture {
        reader,
        input,
        directory,
    } = f;
    let old_id = input.entity_id();
    let weak = input.downgrade();
    drop(input);
    reader.update_in(visual, |reader, window, cx| {
        reader.toggle_source(window, cx)
    });
    for _ in 0..3 {
        visual.run_until_parked();
        visual.update(|window, cx| {
            window.simulate_next_frame(cx);
            window.draw(cx).clear(cx);
        });
    }
    visual.run_until_parked();
    reader.read_with(visual, |reader, _| {
        assert!(reader.editing.is_none());
        assert!(reader.retained_edit.is_some());
    });
    // Do not call focused_input(): the getter used to hide the owning cache
    // by clearing it. Normal unmount/effect cleanup must release the entity.
    assert!(
        weak.upgrade().is_none(),
        "removed source editor retained before focus lookup"
    );
    weak.assert_released();
    assert_eq!(
        released.get(),
        1,
        "actual editor release callback must fire"
    );
    let path = directory.path().join("vault/registry.html");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), prepared);

    reader.update_in(visual, |reader, window, cx| {
        reader.toggle_source(window, cx)
    });
    visual.run_until_parked();
    visual.executor().advance_clock(Duration::from_millis(200));
    visual.run_until_parked();
    let next = reader.read_with(visual, |reader, cx| {
        let input = reader.editing.as_ref().unwrap().input.clone();
        assert_ne!(input.entity_id(), old_id);
        assert_ne!(input.read(cx).source_stamp().document, stamp.document);
        assert_eq!(input.read(cx).value().as_ref(), prepared);
        assert_eq!(input.read(cx).selected_range(), selected);
        input
    });
    visual.dispatch_action(Undo);
    visual.run_until_parked();
    assert_eq!(
        next.read_with(visual, |i, _| i.value().to_string()),
        original
    );
    visual.dispatch_action(Redo);
    visual.run_until_parked();
    assert_eq!(
        next.read_with(visual, |i, _| i.value().to_string()),
        prepared
    );
}
