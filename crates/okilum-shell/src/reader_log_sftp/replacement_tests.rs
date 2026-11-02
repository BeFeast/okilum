use super::*;
use ::core::prelude::v1::test;
use okilum_core::log::stream::Assembler;

#[gpui::test]
fn failed_replacement_retains_sftp_rows_marks_and_source_until_new_snapshot(
    cx: &mut TestAppContext,
) {
    cx.update(crate::test_init);
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
    let view_id = panel.update(visual, |panel, cx| {
        let view = panel.view.as_ref().unwrap();
        view.update(cx, |view, _| {
            assert_eq!(view.total(), 1, "positive control: old result is installed");
            view.selected = Some(0);
            view.anchor = Some(0);
            view.marks.insert((0, 0));
        });
        view.entity_id()
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
    panel.read_with(visual, |panel, cx| {
        assert!(panel.error && !panel.active);
        assert_eq!(panel.message, "Disconnected: synthetic refusal");
        assert_eq!(panel.requested_url.as_deref(), Some(new_url));
        assert_eq!(panel.displayed_url.as_deref(), Some(old_url));
        let view = panel.view.as_ref().unwrap();
        assert_eq!(view.entity_id(), view_id);
        let view = view.read(cx);
        assert_eq!(view.source(0).unwrap().raw(0).unwrap(), b"old row");
        assert_eq!(view.selected, Some(0));
        assert!(view.marks.contains(&(0, 0)));
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
    panel.update(visual, |panel, cx| {
        assert_eq!(panel.displayed_url.as_deref(), Some(new_url));
        assert!(!panel.error && !panel.active);
        let view = panel.view.as_ref().unwrap();
        assert_eq!(view.entity_id(), view_id, "reuse the existing LogView");
        view.update(cx, |view, _| {
            assert_eq!(view.source(0).unwrap().raw(0).unwrap(), b"new row");
            assert!(
                view.selected.is_none() && view.marks.is_empty(),
                "new session identity must not inherit old row marks"
            );
            view.selected = Some(0);
            view.marks.insert((0, 0));
        });
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
    panel.read_with(visual, |panel, cx| {
        let view = panel.view.as_ref().unwrap().read(cx);
        assert_eq!(view.total(), 2);
        assert_eq!(view.source(0).unwrap().raw(1).unwrap(), b"another row");
        assert_eq!(view.selected, Some(0));
        assert!(view.marks.contains(&(0, 0)));
        assert!(
            panel.worker.is_none(),
            "no transport was launched by this UI fixture"
        );
    });
}
