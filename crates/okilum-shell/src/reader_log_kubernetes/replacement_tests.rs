use super::*;
use ::core::prelude::v1::test;
use gpui::TestAppContext;
use okilum_core::log::stream::Assembler;

#[gpui::test]
fn kubernetes_replacement_failure_retains_displayed_target_rows_and_marks(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::test_init(cx);
        reader_log::bind_keys(cx);
    });
    let copy = if cfg!(target_os = "macos") {
        "cmd-c"
    } else {
        "ctrl-c"
    };
    let mark = if cfg!(target_os = "macos") {
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
    visual.simulate_resize(size(px(1200.), px(950.)));
    visual.run_until_parked();
    let panel = panel.unwrap();
    let mut old = Assembler::new("old.log".into(), "old-session".into()).unwrap();
    let old_data = old.append(b"old pod record\n").unwrap().unwrap();
    let old_generation = panel.update_in(visual, |panel, window, cx| {
        for (input, value) in [
            (&panel.context, "old-context"),
            (&panel.namespace, "prod"),
            (&panel.pod, "old-pod"),
            (&panel.container, "server"),
        ] {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        // This is the real Start preparation seam; no transport or kubectl
        // process is constructed by the synthetic UI fixture.
        let request = panel.prepare_start(cx).unwrap();
        assert!(request.args().unwrap().contains(&"pod/old-pod".to_owned()));
        let generation = panel.generation;
        panel.receive(
            generation,
            worker::Update {
                snapshot: Some(old_data.clone()),
                status: worker::Status::Complete,
                diagnostic: String::new(),
                pending: 0,
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
    visual.simulate_keystrokes(mark);
    visual.simulate_keystrokes(copy);
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("log-mark-0").is_some(),
        "positive control: the old row can be marked"
    );
    visual.read(|cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().as_deref(),
            Some("old pod record")
        )
    });

    panel.update_in(visual, |panel, window, cx| {
        for (input, value) in [
            (&panel.context, "new-context"),
            (&panel.namespace, "dev"),
            (&panel.pod, "new-pod"),
            (&panel.container, "other"),
        ] {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        panel.previous = true;
        panel.follow = false;
        let request = panel.prepare_start(cx).unwrap();
        let args = request.args().unwrap();
        assert!(args.contains(&"--context=new-context".to_owned()));
        assert!(args.contains(&"--previous=true".to_owned()));
        assert!(args.contains(&"--follow=false".to_owned()));
        assert!(panel.message.contains("previous result retained"));
        assert_eq!(panel.view.as_ref().unwrap().entity_id(), view_id);
        let generation = panel.generation;
        assert_eq!(generation, old_generation + 1);
        assert!(
            panel.prepare_start(cx).is_none(),
            "one active Start owns the generation"
        );
        assert_eq!(panel.generation, generation);
        panel.stopping = true;
        assert!(
            panel.prepare_start(cx).is_none(),
            "Stop cleanup cannot overlap a new Start"
        );
        panel.receive(
            generation,
            worker::Update {
                snapshot: None,
                status: worker::Status::Failed("Permission denied".into()),
                diagnostic: "kubectl: forbidden".into(),
                pending: 0,
            },
            cx,
        );
        panel.receive(
            old_generation,
            worker::Update {
                snapshot: Some(old_data),
                status: worker::Status::Complete,
                diagnostic: "stale diagnostic".into(),
                pending: 0,
            },
            cx,
        );
    });
    visual.run_until_parked();
    panel.read_with(visual, |panel, _| {
        assert_eq!(panel.error.as_deref(), Some("Permission denied"));
        assert_eq!(panel.diagnostic, "kubectl: forbidden");
        assert!(!panel.running && !panel.stopping);
        assert!(panel
            .displayed_target
            .as_ref()
            .unwrap()
            .contains("old-context · prod/old-pod/server · Current instance"));
        assert!(panel
            .requested_target
            .as_ref()
            .unwrap()
            .contains("new-context · dev/new-pod/other · Previous instance"));
        assert_eq!(panel.view.as_ref().unwrap().entity_id(), view_id);
    });
    assert!(visual.debug_bounds("log-mark-0").is_some());
    visual.simulate_keystrokes(copy);
    visual.read(|cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().as_deref(),
            Some("old pod record")
        )
    });

    let mut replacement = Assembler::new("new.log".into(), "new-session".into()).unwrap();
    let new_data = replacement
        .append(b"previous instance record\n")
        .unwrap()
        .unwrap();
    panel.update(visual, |panel, cx| {
        assert!(panel.prepare_start(cx).is_some());
        panel.receive(
            panel.generation,
            worker::Update {
                snapshot: Some(new_data),
                status: worker::Status::Complete,
                diagnostic: String::new(),
                pending: 0,
            },
            cx,
        );
    });
    visual.run_until_parked();
    panel.read_with(visual, |panel, _| {
        assert_eq!(panel.displayed_target, panel.requested_target);
        assert!(panel.error.is_none() && panel.diagnostic.is_empty());
        assert_eq!(panel.view.as_ref().unwrap().entity_id(), view_id);
        assert!(panel.worker.is_none(), "no kubectl was launched");
    });
    assert!(
        visual.debug_bounds("log-mark-0").is_none(),
        "new session must not inherit old marks"
    );
    visual.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("no selection".into())));
    visual.simulate_keystrokes(copy);
    visual.read(|cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().as_deref(),
            Some("no selection")
        )
    });
    visual.simulate_keystrokes("down");
    visual.simulate_keystrokes(copy);
    visual.read(|cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().as_deref(),
            Some("previous instance record")
        )
    });
}
