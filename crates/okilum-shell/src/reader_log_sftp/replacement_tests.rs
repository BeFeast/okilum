use super::*;
use ::core::prelude::v1::test;
use okilum_core::log::stream::Assembler;

#[gpui::test]
fn failed_replacement_retains_sftp_rows_marks_and_source_until_new_snapshot(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| {
        crate::test_init(cx);
        reader_log::bind_keys(cx);
    });
    let copy_key = if cfg!(target_os = "macos") {
        "cmd-c"
    } else {
        "ctrl-c"
    };
    let mark_key = if cfg!(target_os = "macos") {
        "cmd-f2"
    } else {
        "ctrl-f2"
    };
    let mut panel = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| Panel::new("fixture".into(), window, cx));
        panel = Some(entity.clone());
        Root::new(entity, window, cx)
    });
    visual.simulate_resize(size(px(1200.), px(850.)));
    visual.run_until_parked();
    let panel = panel.unwrap();
    let old_url = "sftp://old-alias/var/old.log";
    let new_url = "sftp://new-alias/var/new.log";
    let mut old = Assembler::new("old.log".into(), "old-session".into()).unwrap();
    let old_data = old.append(b"old row\n").unwrap().unwrap();
    let old_generation = panel.update_in(visual, |panel, window, cx| {
        panel
            .url
            .update(cx, |input, cx| input.set_value(old_url, window, cx));
        // Exercise the same UI preparation used by Start, with no transport
        // construction or actual OpenSSH process in this synthetic fixture.
        assert!(panel.prepare_start(cx).is_some());
        let generation = panel.generation;
        panel.receive(
            generation,
            worker::Frame {
                data: Some(old_data.clone()),
                message: "Loaded snapshot".into(),
                active: false,
                error: false,
            },
            cx,
        );
        generation
    });
    visual.run_until_parked();
    let view_id = panel.update_in(visual, |panel, window, cx| {
        let view = panel.view.as_ref().unwrap();
        let focus = view.read(cx).focus_handle().clone();
        focus.focus(window, cx);
        view.entity_id()
    });
    visual.simulate_keystrokes("down");
    visual.simulate_keystrokes(mark_key);
    visual.simulate_keystrokes(copy_key);
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("log-mark-0").is_some(),
        "positive control: UI created a mark"
    );
    visual.read(|cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().as_deref(),
            Some("old row")
        )
    });
    panel.update_in(visual, |panel, window, cx| {
        panel
            .url
            .update(cx, |input, cx| input.set_value(new_url, window, cx));
        assert!(panel.prepare_start(cx).is_some());
        assert_eq!(panel.generation, old_generation + 1);
        assert!(panel.active);
        assert!(panel.message.contains("previous session rows retained"));
        assert_eq!(panel.view.as_ref().unwrap().entity_id(), view_id);
        let generation = panel.generation;
        assert!(
            panel.prepare_start(cx).is_none(),
            "duplicate Start is inert while active"
        );
        assert_eq!(panel.generation, generation);
        panel.receive(
            generation,
            worker::Frame {
                data: None,
                message: "Disconnected: synthetic refusal".into(),
                active: false,
                error: true,
            },
            cx,
        );
        // An earlier session completing late cannot replace data or mask the
        // new attempt's error/terminal state.
        panel.receive(
            old_generation,
            worker::Frame {
                data: Some(old_data.clone()),
                message: "Stale success".into(),
                active: true,
                error: false,
            },
            cx,
        );
    });
    visual.run_until_parked();
    panel.read_with(visual, |panel, _| {
        assert!(panel.error && !panel.active);
        assert_eq!(panel.message, "Disconnected: synthetic refusal");
        assert_eq!(panel.requested_url.as_deref(), Some(new_url));
        assert_eq!(panel.displayed_url.as_deref(), Some(old_url));
        let view = panel.view.as_ref().unwrap();
        assert_eq!(view.entity_id(), view_id);
    });
    assert!(visual.debug_bounds("log-mark-0").is_some());
    visual.simulate_keystrokes(copy_key);
    visual.read(|cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().as_deref(),
            Some("old row")
        )
    });

    let mut new = Assembler::new("new.log".into(), "new-session".into()).unwrap();
    let new_data = new.append(b"new row\n").unwrap().unwrap();
    panel.update(visual, |panel, cx| {
        assert!(panel.prepare_start(cx).is_some());
        panel.receive(
            panel.generation,
            worker::Frame {
                data: Some(new_data),
                message: "Loaded replacement".into(),
                active: false,
                error: false,
            },
            cx,
        );
    });
    visual.run_until_parked();
    panel.read_with(visual, |panel, _| {
        assert_eq!(panel.displayed_url.as_deref(), Some(new_url));
        assert!(!panel.error && !panel.active);
        let view = panel.view.as_ref().unwrap();
        assert_eq!(view.entity_id(), view_id, "reuse the existing LogView");
    });
    assert!(
        visual.debug_bounds("log-mark-0").is_none(),
        "new session must not inherit old marks"
    );
    visual.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("no selection")));
    visual.simulate_keystrokes(copy_key);
    visual.read(|cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().as_deref(),
            Some("no selection")
        )
    });
    visual.simulate_keystrokes("down");
    visual.simulate_keystrokes(mark_key);
    visual.simulate_keystrokes(copy_key);
    visual.read(|cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().as_deref(),
            Some("new row")
        )
    });
    panel.update(visual, |panel, cx| {
        // Preserve a successful snapshot queued for paint too, if the next
        // explicit request fails before any of its own rows arrive.
        let appended = new.append(b"another row\n").unwrap().unwrap();
        panel.receive(
            panel.generation,
            worker::Frame {
                data: Some(appended),
                message: "Latest rows".into(),
                active: false,
                error: false,
            },
            cx,
        );
        assert!(panel.prepare_start(cx).is_some());
        assert!(panel.pending.is_some());
        panel.receive(
            panel.generation,
            worker::Frame {
                data: None,
                message: "Replacement failed".into(),
                active: false,
                error: true,
            },
            cx,
        );
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("log-mark-0").is_some());
    visual.simulate_keystrokes(copy_key);
    visual.read(|cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().as_deref(),
            Some("new row")
        )
    });
    visual.simulate_keystrokes("down");
    visual.simulate_keystrokes(copy_key);
    visual.read(|cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().as_deref(),
            Some("another row")
        )
    });
    panel.read_with(visual, |panel, _| {
        assert!(
            panel.worker.is_none(),
            "no transport was launched by this UI fixture"
        );
    });
}
