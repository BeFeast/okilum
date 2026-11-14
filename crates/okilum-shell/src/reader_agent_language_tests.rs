//! #1593: inline language marks and term hover in the source editor, and a
//! policy-only change refreshing them without a source edit. #2953: the same
//! hover in Live Preview.
use super::*;
use ::core::prelude::v1::test;

const SOURCE: &str = "# Rules\n\nВоркер мержит.\n\n`воркер`\n";
const POLICY: &str = "backlog:\n  - pattern: 'воркер'\n    good: worker\n";

fn replacements(reader: &Reader) -> Option<Vec<String>> {
    let published = reader.agent_language.as_ref()?.published()?;
    Some(
        published
            .report
            .diagnostics
            .iter()
            .map(|d| d.replacement.clone())
            .collect(),
    )
}

/// The checker runs on its own thread; drive the UI until it publishes.
fn settle(
    reader: &Entity<Reader>,
    visual: &mut VisualTestContext,
    until: impl Fn(&Reader) -> bool,
) -> bool {
    for _ in 0..300 {
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        if reader.read_with(visual, |r, _| until(r)) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

#[gpui::test]
fn issue1593_marks_hover_and_policy_refresh_without_a_source_edit(cx: &mut TestAppContext) {
    // The checker publishes from its own `agent-language` thread.
    cx.executor().allow_parking();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("vault");
    std::fs::create_dir_all(root.join("project")).unwrap();
    std::fs::create_dir_all(root.join("@Schema")).unwrap();
    std::fs::write(root.join("project/AGENTS.md"), SOURCE).unwrap();
    std::fs::write(root.join("@Schema/language.yaml"), POLICY).unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
        cx.set_reduce_motion(true);
    });
    let mut entity = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let reader = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("project/AGENTS.md".into()),
                    session_directory: Some(directory.path().join("state")),
                    index_dir: Some(directory.path().join("index")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        entity = Some(reader.clone());
        Root::new(reader, window, cx)
    });
    let reader = entity.unwrap();
    visual.simulate_resize(size(px(1400.), px(960.)));
    reader.update_in(visual, |r, window, cx| r.toggle_source(window, cx));
    assert!(
        settle(&reader, visual, |r| replacements(r)
            .is_some_and(|v| v.len() == 1)),
        "positive control: the vault policy finds the prose term only"
    );
    let input = reader.read_with(visual, |r, _| r.source_input().unwrap());
    let term = SOURCE.find("Воркер").unwrap();
    let term_range = term..term + "Воркер".len();
    let marked = reader.read_with(visual, |r, cx| {
        r.agent_language_marks
            .as_ref()
            .map(|marks| marks.collection.get_ranges(cx))
    });
    assert_eq!(
        marked,
        Some(vec![term_range.clone()]),
        "inline mark on the term"
    );

    let provider = reader_image_actions::source_hover(reader.downgrade());
    let text = input.read_with(visual, |input, _| input.text().clone());
    let hover = visual.update(|window, cx| provider.hover(&text, term + 2, window, cx));
    let hover = futures::executor::block_on(hover)
        .unwrap()
        .expect("term hover");
    let lsp_types::HoverContents::Markup(markup) = hover.contents else {
        panic!("plain text hover");
    };
    assert!(
        markup.value.contains("Latin form: worker"),
        "{}",
        markup.value
    );

    // Policy-only change: the source stays byte-identical. The watcher's
    // delivery of this write is covered in okilum-core watch tests.
    std::fs::write(
        root.join("@Schema/language.yaml"),
        "backlog:\n  - pattern: 'воркер'\n    good: worker-new\n  - pattern: 'мерж\\w*'\n    good: merge\n",
    )
    .unwrap();
    reader.update(visual, |r, cx| r.agent_language_policy_changed(cx));
    assert!(
        settle(&reader, visual, |r| replacements(r)
            == Some(vec!["worker-new".to_string(), "merge".to_string()])),
        "the open file's check uses the new policy without a source edit"
    );
    // Marks are applied from the published result on a later frame; wait for
    // them too, so a mark that never follows the policy still fails here.
    let marks = |r: &Reader, cx: &App| {
        r.agent_language_marks
            .as_ref()
            .map(|marks| marks.collection.get_ranges(cx).len())
    };
    let mut marked = None;
    for _ in 0..300 {
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        marked = reader.read_with(visual, |r, cx| marks(r, cx));
        if marked == Some(2) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(marked, Some(2), "marks follow the new policy");
    assert_eq!(
        std::fs::read_to_string(root.join("project/AGENTS.md")).unwrap(),
        SOURCE
    );
}

/// Drive the UI until the editor shows `count` language marks.
fn settle_marks(reader: &Entity<Reader>, visual: &mut VisualTestContext, count: usize) -> bool {
    for _ in 0..300 {
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let marked = reader.read_with(visual, |r, cx| {
            r.agent_language_marks
                .as_ref()
                .map_or(0, |marks| marks.collection.get_ranges(cx).len())
        });
        if marked == count {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

/// Run the editor's installed hover to completion; `None` when nothing shows.
fn hover_text(
    visual: &mut VisualTestContext,
    input: &Entity<gpui_component::input::EditorState>,
    text: &gpui_component::input::Rope,
    offset: usize,
) -> Option<String> {
    let provider = input
        .read_with(visual, |input, _| input.lsp().hover_provider.clone())
        .expect("Live installs the native hover over marked terms");
    let result = Rc::new(std::cell::RefCell::new(None));
    let output = result.clone();
    visual.update(|window, cx| {
        let task = provider.hover(text, offset, window, cx);
        cx.spawn(async move |_| {
            *output.borrow_mut() = Some(task.await.unwrap());
        })
        .detach();
    });
    visual.run_until_parked();
    visual.executor().advance_clock(Duration::from_millis(500));
    visual.run_until_parked();
    let hover = result.borrow_mut().take().expect("hover task ran")?;
    let lsp_types::HoverContents::Markup(markup) = hover.contents else {
        panic!("plain text hover");
    };
    Some(markup.value)
}

#[gpui::test]
fn issue2953_live_preview_hover_shows_the_latin_form(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("vault");
    std::fs::create_dir_all(root.join("project")).unwrap();
    std::fs::create_dir_all(root.join("@Schema")).unwrap();
    std::fs::write(root.join("project/AGENTS.md"), SOURCE).unwrap();
    std::fs::write(root.join("@Schema/language.yaml"), POLICY).unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
        cx.set_reduce_motion(true);
    });
    let mut entity = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let reader = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("project/AGENTS.md".into()),
                    session_directory: Some(directory.path().join("state")),
                    index_dir: Some(directory.path().join("index")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        entity = Some(reader.clone());
        Root::new(reader, window, cx)
    });
    let reader = entity.unwrap();
    visual.simulate_resize(size(px(1400.), px(960.)));
    reader.update_in(visual, |r, window, cx| r.open_live_preview(window, cx));
    // Marks published while Live is open install the hover.
    assert!(
        settle_marks(&reader, visual, 1),
        "positive control: the term is marked in Live Preview"
    );
    let input = reader.read_with(visual, |r, _| r.source_input().unwrap());
    let text = input.read_with(visual, |input, _| input.text().clone());
    let term = SOURCE.find("Воркер").unwrap();
    let hover = hover_text(visual, &input, &text, term + 2).expect("term hover in Live");
    assert!(hover.contains("Latin form: worker"), "{hover}");
    // An unmarked word and text other than the checked one show nothing.
    let unmarked = SOURCE.find("мержит").unwrap() + 2;
    assert_eq!(hover_text(visual, &input, &text, unmarked), None);
    let stale = gpui_component::input::Rope::from(SOURCE.replace("# Rules", "# Rulez").as_str());
    assert_eq!(hover_text(visual, &input, &stale, term + 2), None);

    // The real pointer reaches the hover over the marked term in Live.
    let point = input.read_with(visual, |input, _| {
        input
            .range_to_bounds(&(term..term + "Воркер".len()))
            .expect("marked term has native bounds")
            .center()
    });
    visual.simulate_mouse_move(point, None, Modifiers::default());
    visual.run_until_parked();
    visual.executor().advance_clock(Duration::from_millis(500));
    visual.run_until_parked();
    for _ in 0..3 {
        visual.update(|window, cx| window.draw(cx).clear(cx));
        visual.run_until_parked();
    }
    input.read_with(visual, |input, _| {
        assert!(input.hover_popover().is_some(), "Live shows the term hover")
    });

    // Without marks Live keeps its projection: no native hover is installed.
    std::fs::write(
        root.join("@Schema/language.yaml"),
        "backlog:\n  - pattern: 'absent'\n    good: present\n",
    )
    .unwrap();
    reader.update(visual, |r, cx| r.agent_language_policy_changed(cx));
    assert!(
        settle_marks(&reader, visual, 0),
        "a policy without matches clears the marks"
    );
    input.read_with(visual, |input, _| {
        assert!(
            input.lsp().hover_provider.is_none(),
            "unmarked Live has no hover"
        );
        assert!(input.hover_popover().is_none(), "no stale term popover");
    });
    // Leaving and re-entering Live with marks installs it on the toggle path.
    std::fs::write(root.join("@Schema/language.yaml"), POLICY).unwrap();
    reader.update(visual, |r, cx| r.agent_language_policy_changed(cx));
    assert!(settle_marks(&reader, visual, 1), "marks return");
    reader.update_in(visual, |r, window, cx| r.toggle_live_preview(window, cx));
    reader.update_in(visual, |r, window, cx| r.toggle_live_preview(window, cx));
    visual.run_until_parked();
    let hover = hover_text(visual, &input, &text, term + 2).expect("term hover after re-entry");
    assert!(hover.contains("Latin form: worker"), "{hover}");
    assert_eq!(
        std::fs::read_to_string(root.join("project/AGENTS.md")).unwrap(),
        SOURCE
    );
}

/// Move the real pointer onto `range` and return the popup it shows.
fn pointer_hover(
    visual: &mut VisualTestContext,
    input: &Entity<gpui_component::input::EditorState>,
    range: std::ops::Range<usize>,
) -> Option<String> {
    let point = input.read_with(visual, |input, _| {
        input
            .range_to_bounds(&range)
            .expect("term has native bounds")
            .center()
    });
    visual.simulate_mouse_move(point, None, Modifiers::default());
    visual.run_until_parked();
    visual.executor().advance_clock(Duration::from_millis(500));
    visual.run_until_parked();
    for _ in 0..3 {
        visual.update(|window, cx| window.draw(cx).clear(cx));
        visual.run_until_parked();
    }
    popup(visual, input)
}

fn popup(
    visual: &mut VisualTestContext,
    input: &Entity<gpui_component::input::EditorState>,
) -> Option<String> {
    input.read_with(visual, |input, _| {
        input
            .hover_popover()
            .map(|popover| match &popover.hover.contents {
                lsp_types::HoverContents::Markup(markup) => markup.value.clone(),
                other => format!("{other:?}"),
            })
    })
}

#[gpui::test]
fn issue2953_hover_never_outlives_its_text_or_publication(cx: &mut TestAppContext) {
    const LIVE: &str = "# Rules\n\nВоркер мержит.\n\nЭто **воркер** в тексте.\n\n`воркер`\n";
    cx.executor().allow_parking();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("vault");
    std::fs::create_dir_all(root.join("project")).unwrap();
    std::fs::create_dir_all(root.join("@Schema")).unwrap();
    std::fs::write(root.join("project/AGENTS.md"), LIVE).unwrap();
    std::fs::write(root.join("@Schema/language.yaml"), POLICY).unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
        cx.set_reduce_motion(true);
    });
    let mut entity = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let reader = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("project/AGENTS.md".into()),
                    session_directory: Some(directory.path().join("state")),
                    index_dir: Some(directory.path().join("index")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        entity = Some(reader.clone());
        Root::new(reader, window, cx)
    });
    let reader = entity.unwrap();
    visual.simulate_resize(size(px(1400.), px(960.)));
    reader.update_in(visual, |r, window, cx| r.open_live_preview(window, cx));
    assert!(
        settle_marks(&reader, visual, 2),
        "positive control: the plain and the bold term are marked"
    );
    let input = reader.read_with(visual, |r, _| r.source_input().unwrap());
    let text = input.read_with(visual, |input, _| input.text().clone());
    let bold = LIVE.find("воркер**").unwrap();
    let hover = hover_text(visual, &input, &text, bold + 2).expect("bold term hover");
    assert!(hover.contains("Latin form: worker"), "{hover}");
    let term = LIVE.find("Воркер").unwrap();
    let range = term..term + "Воркер".len();
    let old = "Latin form: worker\n";

    // Case 1: a policy-only change never leaves the old form on screen.
    let shown = pointer_hover(visual, &input, range.clone()).expect("Live term popup");
    assert!(shown.contains(old), "{shown}");
    std::fs::write(
        root.join("@Schema/language.yaml"),
        "backlog:\n  - pattern: 'воркер'\n    good: worker-new\n",
    )
    .unwrap();
    reader.update(visual, |r, cx| r.agent_language_policy_changed(cx));
    assert!(
        settle(&reader, visual, |r| replacements(r)
            == Some(vec![
                "worker-new".to_string(),
                "worker-new".to_string()
            ])),
        "the new policy is published"
    );
    for _ in 0..3 {
        visual.update(|window, cx| window.draw(cx).clear(cx));
        visual.run_until_parked();
    }
    if let Some(shown) = popup(visual, &input) {
        assert!(
            !shown.contains(old),
            "stale Latin form after policy change: {shown}"
        );
    }
    let shown = pointer_hover(visual, &input, range.start + 2..range.end)
        .expect("the term hovers again with the new publication");
    assert!(shown.contains("Latin form: worker-new"), "{shown}");

    // Case 2: a keyboard edit drops the popup of the text before it.
    visual.simulate_input("x");
    visual.run_until_parked();
    assert_ne!(
        input.read_with(visual, |input, _| input.text().to_string()),
        LIVE,
        "positive control: the keystroke edited the source"
    );
    assert_eq!(popup(visual, &input), None, "Live popup survives an edit");

    // The same lifecycle holds in Source.
    reader.update_in(visual, |r, window, cx| r.toggle_live_preview(window, cx));
    visual.run_until_parked();
    assert!(
        settle(&reader, visual, |r| replacements(r)
            .is_some_and(|v| !v.is_empty())),
        "the edited source is checked"
    );
    let current = input.read_with(visual, |input, _| input.text().to_string());
    let term = current.find("Воркер").unwrap();
    let shown =
        pointer_hover(visual, &input, term..term + "Воркер".len()).expect("Source term popup");
    assert!(shown.contains("Latin form: worker-new"), "{shown}");
    visual.simulate_input("y");
    visual.run_until_parked();
    assert_eq!(popup(visual, &input), None, "Source popup survives an edit");
}
