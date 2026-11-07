//! Exercise actual Reader dispatch, inline media, Canvas input and note navigation.
use super::*;
use ::core::prelude::v1::test;

#[gpui::test]
fn local_viewers_dispatch_inline_and_as_files_and_canvas_navigates_without_writes(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("vault");
    std::fs::create_dir_all(root.join("canvas")).unwrap();
    // Match production vault admission, including macOS /var -> /private/var.
    let root = root.canonicalize().unwrap();
    let canvas = include_bytes!("../../okilum-core/tests/fixtures/viewers/canvas/read-only.canvas");
    std::fs::write(root.join("canvas/read-only.canvas"), canvas).unwrap();
    std::fs::write(
        root.join("canvas/linked-note.md"),
        "# Linked note\n\n## Acceptance\n\nKeep this note.\n",
    )
    .unwrap();
    std::fs::write(
        root.join("note.md"),
        "# Media\n\n![[clip.mp4]]\n\n![[tone.mp3]]\n\n![[tone.wav]]\n",
    )
    .unwrap();
    std::fs::write(root.join("clip.mp4"), b"synthetic video identity").unwrap();
    for file in ["tone.mp3", "tone.wav"] {
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../okilum-core/tests/fixtures/viewers/audio")
                .join(file),
            root.join(file),
        )
        .unwrap();
    }
    cx.update(|cx| {
        crate::test_init(cx);
        reader_ui_state::install(&dir.path().join("state"), cx);
        bind_keys(cx);
    });
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("note.md".into()),
                    index_dir: Some(dir.path().join("index")),
                    session_directory: Some(dir.path().join("state")),
                    panel_settings_override: Some(dir.path().join("panels.json")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        reader = Some(entity.clone());
        Root::new(entity, window, cx)
    });
    let reader = reader.unwrap();
    visual.simulate_resize(size(px(1400.), px(1000.)));
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("video-open-system").is_some(),
        "inline video must render its action"
    );
    assert!(
        visual.debug_bounds("audio-player").is_some(),
        "inline audio must render a player"
    );
    assert!(
        visual.opened_url().is_none(),
        "rendering never launches a system app"
    );
    for file in ["tone.mp3", "tone.wav"] {
        reader.update_in(visual, |r, window, cx| {
            r.preview_file(file, window, cx);
            assert!(r.file_preview.as_ref().unwrap().audio.is_some());
            r.toggle_source(window, cx);
            assert!(r.editing.is_none());
        });
        visual.run_until_parked();
        assert!(visual.debug_bounds("audio-toggle").is_some());
    }
    reader.update_in(visual, |r, window, cx| {
        r.preview_file("clip.mp4", window, cx)
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("video-open-system").is_some());
    reader.update_in(visual, |r, window, cx| {
        r.preview_file("canvas/read-only.canvas", window, cx)
    });
    visual.run_until_parked();
    let view = reader.read_with(visual, |r, _| {
        r.file_preview.as_ref().unwrap().canvas.clone().unwrap()
    });
    // Test windows do not run a platform frame loop. Deliver the frame requested
    // by first measurement, without simulating an input or pressing Fit.
    visual.update(|window, cx| window.simulate_next_frame(cx));
    visual.run_until_parked();
    // The first frame must fit negative scene coordinates without a user input.
    // Previously the measure callback notified during drawing, which left the
    // native window showing only the group border until Fit or another event.
    let initial_area = visual.debug_bounds("canvas-scene").unwrap();
    assert!(initial_area.size.width > px(200.) && initial_area.size.height > px(200.));
    let initial_note = visual
        .debug_bounds("canvas-open-note-note")
        .expect("a file card must be visible before the first user input");
    assert!(initial_area.contains(&initial_note.center()));
    let scale = view.read_with(visual, |v, _| {
        assert!(matches!(v.scene, Some(Ok(_))));
        v.view.scale
    });
    let plus = visual
        .debug_bounds("canvas-plus")
        .expect("zoom button must render");
    visual.simulate_click(plus.center(), Modifiers::default());
    visual.run_until_parked();
    view.read_with(visual, |v, _| assert!(v.view.scale > scale));
    let fit = visual.debug_bounds("canvas-fit").unwrap();
    visual.simulate_click(fit.center(), Modifiers::default());
    visual.run_until_parked();
    let area = visual.debug_bounds("canvas-scene").unwrap();
    let before = view.read_with(visual, |v, _| v.view.x);
    visual.simulate_event(ScrollWheelEvent {
        position: area.center(),
        delta: ScrollDelta::Pixels(point(px(36.), px(24.))),
        modifiers: Modifiers::default(),
        touch_phase: TouchPhase::Moved,
    });
    visual.run_until_parked();
    view.read_with(visual, |v, _| {
        assert!(v.view.x > before, "wheel must pan the actual scene")
    });
    // The Reader's capture-phase text zoom must yield to Canvas even when
    // ordinary note text is already at its own maximum zoom.
    let original_text_zoom = visual.update(|_, cx| {
        let zoom = reader_ui_state::text_zoom(cx);
        reader_ui_state::set_text_zoom(3., cx);
        assert_eq!(
            reader_ui_state::text_zoom(cx),
            3.,
            "the zoom-limit control must be active"
        );
        zoom
    });
    let before = view.read_with(visual, |v, _| v.view.scale);
    visual.simulate_event(ScrollWheelEvent {
        position: area.center(),
        delta: ScrollDelta::Pixels(point(px(0.), px(64.))),
        modifiers: Modifiers {
            control: true,
            platform: true,
            ..Default::default()
        },
        touch_phase: TouchPhase::Moved,
    });
    visual.run_until_parked();
    view.read_with(visual, |v, _| assert!(v.view.scale > before));
    visual.update(|_, cx| {
        assert_eq!(reader_ui_state::text_zoom(cx), 3.);
        reader_ui_state::set_text_zoom(original_text_zoom, cx);
    });
    let note = visual
        .debug_bounds("canvas-open-note-note")
        .expect("existing file card must render");
    visual.simulate_click(note.center(), Modifiers::default());
    visual.run_until_parked();
    reader.read_with(visual, |r, _| {
        assert_eq!(r.selected_file(), "canvas/linked-note.md")
    });
    assert_eq!(
        std::fs::read(root.join("canvas/read-only.canvas")).unwrap(),
        canvas
    );
    assert!(visual.opened_url().is_none());
}

#[gpui::test]
fn cold_start_canvas_opens_its_exact_note_before_another_document(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("vault");
    std::fs::create_dir_all(root.join("canvas")).unwrap();
    let root = root.canonicalize().unwrap();
    let canvas = include_bytes!("../../okilum-core/tests/fixtures/viewers/canvas/read-only.canvas");
    let note = b"# Linked note\n\n## Acceptance\n\nNever change this source.\n";
    std::fs::write(root.join("canvas/read-only.canvas"), canvas).unwrap();
    std::fs::write(root.join("canvas/linked-note.md"), note).unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        reader_ui_state::install(&dir.path().join("state"), cx);
        bind_keys(cx);
    });
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("canvas/read-only.canvas".into()),
                    index_dir: Some(dir.path().join("index")),
                    session_directory: Some(dir.path().join("state")),
                    panel_settings_override: Some(dir.path().join("panels.json")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        reader = Some(entity.clone());
        Root::new(entity, window, cx)
    });
    let reader = reader.unwrap();
    visual.simulate_resize(size(px(1400.), px(1000.)));
    visual.run_until_parked();
    visual.update(|window, cx| window.simulate_next_frame(cx));
    visual.run_until_parked();
    reader.read_with(visual, |r, _| {
        assert_eq!(r.selected_file(), "canvas/read-only.canvas");
        assert!(r.vault.notes.contains(&"canvas/linked-note.md".to_owned()));
    });
    let button = visual.debug_bounds("canvas-open-note-note").unwrap();
    visual.simulate_click(button.center(), Modifiers::default());
    visual.run_until_parked();
    reader.read_with(visual, |r, _| {
        assert_eq!(
            r.selected_file(),
            "canvas/linked-note.md",
            "the cold Canvas must not retain its incomplete startup inventory"
        );
    });
    assert!(visual.opened_url().is_none());
    assert_eq!(
        std::fs::read(root.join("canvas/read-only.canvas")).unwrap(),
        canvas
    );
    assert_eq!(
        std::fs::read(root.join("canvas/linked-note.md")).unwrap(),
        note
    );
}
