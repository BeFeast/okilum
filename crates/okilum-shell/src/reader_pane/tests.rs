//! Stage 2 of #773: late document work stays bound to its pane and generation.
//! One pane is visible; replacing its note and closing it (a fresh surface in
//! its place) are the two ways a completion becomes stale.
use crate::*;
use ::core::prelude::v1::test;

fn settle(visual: &mut gpui::VisualTestContext) {
    visual.run_until_parked();
    visual.executor().advance_clock(Duration::from_millis(300));
    visual.run_until_parked();
}

fn fixture(
    cx: &mut gpui::TestAppContext,
) -> (
    Entity<Reader>,
    &mut gpui::VisualTestContext,
    tempfile::TempDir,
) {
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    super::DROPPED_FOR_CLOSED_PANE.with(|dropped| dropped.borrow_mut().clear());
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("notes");
    std::fs::create_dir(&root).unwrap();
    for name in ["A", "B", "C"] {
        std::fs::write(
            root.join(format!("{name}.md")),
            format!("# {name}\n\nBody {name}.\n"),
        )
        .unwrap();
    }
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("A.md".into()),
                    session_directory: Some(fixture.path().join("state")),
                    index_dir: Some(fixture.path().join("index")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        reader = Some(view.clone());
        Root::new(view, window, cx)
    });
    let reader = reader.unwrap();
    visual.simulate_resize(size(px(1200.), px(800.)));
    settle(visual);
    reader.read_with(visual, |r, _| {
        assert_eq!(
            r.pane.current_rel, "A.md",
            "positive control: fixture opened"
        );
        assert!(r.pane.refused.is_empty());
    });
    (reader, visual, fixture)
}

/// Starts opening `rel` and holds its preparation before it may publish.
fn open_held(
    reader: &Entity<Reader>,
    visual: &mut gpui::VisualTestContext,
    rel: &str,
) -> async_channel::Sender<()> {
    let (release, hold) = async_channel::bounded(1);
    reader.update_in(visual, |r, window, cx| {
        r.pane.preparation_hold = Some(hold);
        r.open_note(rel, None, window, cx);
    });
    settle(visual);
    reader.read_with(visual, |r, _| {
        assert_eq!(
            r.pane.current_rel, "A.md",
            "held preparation has not published"
        );
    });
    release
}

#[gpui::test]
fn pane773_held_preparation_publishes_when_still_current(cx: &mut gpui::TestAppContext) {
    // Positive control for the hold itself: nothing replaced, so it lands.
    let (reader, visual, _dir) = fixture(cx);
    let release = open_held(&reader, visual, "B.md");
    release.try_send(()).unwrap();
    settle(visual);
    reader.read_with(visual, |r, _| {
        assert_eq!(r.pane.current_rel, "B.md");
        assert!(
            r.pane.refused.is_empty(),
            "a current completion is not refused"
        );
    });
}

#[gpui::test]
fn pane773_late_preparation_after_replace_is_dropped(cx: &mut gpui::TestAppContext) {
    let (reader, visual, _dir) = fixture(cx);
    let release = open_held(&reader, visual, "B.md");
    let stale = reader.read_with(visual, |r, _| r.pane.preparation_ticket());
    reader.update_in(visual, |r, window, cx| {
        r.open_note("C.md", None, window, cx)
    });
    settle(visual);
    reader.read_with(visual, |r, _| {
        assert_eq!(r.pane.current_rel, "C.md", "replacement published");
        assert!(r.pane.refused.is_empty(), "B has not completed yet");
    });
    release.try_send(()).unwrap();
    settle(visual);
    reader.read_with(visual, |r, _| {
        assert_eq!(
            r.pane.refused,
            vec![stale],
            "the late B completion was observed"
        );
        assert_eq!(r.pane.current_rel, "C.md", "and dropped");
        assert!(r.pane.note_source.contains("Body C."));
    });
}

#[gpui::test]
fn pane773_late_preparation_after_close_is_dropped(cx: &mut gpui::TestAppContext) {
    let (reader, visual, _dir) = fixture(cx);
    let release = open_held(&reader, visual, "B.md");
    let stale = reader.read_with(visual, |r, _| r.pane.preparation_ticket());
    reader.update(visual, |r, _| {
        let generations = (
            r.pane.navigation.generation,
            r.pane.navigation.preparation_generation,
        );
        r.pane.reopen_as_new_pane();
        assert_eq!(
            generations,
            (
                r.pane.navigation.generation,
                r.pane.navigation.preparation_generation
            ),
            "same generations: only the pane identity differs",
        );
        assert_ne!(r.pane.preparation_ticket(), stale);
    });
    release.try_send(()).unwrap();
    settle(visual);
    reader.read_with(visual, |r, _| {
        // The completion fired and was dropped at the routing boundary for
        // the closed pane, so no ticket check of the new pane ever saw it.
        assert_eq!(
            super::DROPPED_FOR_CLOSED_PANE.with(|dropped| dropped.borrow().clone()),
            vec![stale.pane()],
            "the late completion was observed and dropped for its closed pane"
        );
        assert!(r.pane.refused.is_empty(), "the new pane never checked it");
        assert_eq!(
            r.pane.current_rel, "A.md",
            "and never published into the new pane"
        );
        assert!(r.pane.note_source.contains("Body A."));
    });
}

#[gpui::test]
fn pane773_navigation_ticket_needs_pane_and_generation(cx: &mut gpui::TestAppContext) {
    let (reader, visual, _dir) = fixture(cx);
    reader.update(visual, |r, _| {
        let ticket = r.pane.navigation_ticket();
        assert!(r.pane.holds_navigation(ticket), "positive control");
        r.pane.navigation.generation = r.pane.navigation.generation.wrapping_add(1);
        assert!(!r.pane.holds_navigation(ticket), "newer navigation");
        let current = r.pane.navigation_ticket();
        r.pane.reopen_as_new_pane();
        assert!(!r.pane.holds_navigation(current), "another pane");
        assert_eq!(r.pane.refused, vec![ticket, current]);
    });
}

#[gpui::test]
fn pane773_landing_for_another_pane_leaves_this_one_alone(cx: &mut gpui::TestAppContext) {
    let (reader, visual, _dir) = fixture(cx);
    let start = ListOffset {
        item_ix: 0,
        offset_in_item: px(0.),
    };
    let marker = ListOffset {
        item_ix: 1,
        offset_in_item: px(0.),
    };
    for reopen in [false, true] {
        reader.update(visual, |r, cx| {
            r.scroll_to_position(start, cx);
            // Another landing with the same landing generation, as a second
            // pane would have after its own navigation.
            r.pane.navigation.pending_landing = Some(marker);
            if reopen {
                r.pane.reopen_as_new_pane();
            } else {
                r.pane.navigation.generation = r.pane.navigation.generation.wrapping_add(1);
            }
        });
        visual.executor().advance_clock(Duration::from_millis(120));
        visual.run_until_parked();
        reader.update(visual, |r, _| {
            if reopen {
                assert_eq!(
                    r.pane.navigation.pending_landing.map(|p| p.item_ix),
                    Some(marker.item_ix),
                    "a landing for another pane leaves this one alone"
                );
            } else {
                assert!(
                    r.pane.navigation.pending_landing.is_none(),
                    "positive control: a stale landing in the same pane is cleared"
                );
            }
            r.pane.navigation.pending_landing = None;
        });
    }
}

#[gpui::test]
fn pane773_completions_run_for_their_own_pane(cx: &mut gpui::TestAppContext) {
    let (reader, visual, _dir) = fixture(cx);
    reader.update_in(visual, |r, window, cx| {
        let ran = std::rc::Rc::new(std::cell::RefCell::new(Vec::<&str>::new()));
        let log = |label: &'static str| {
            let ran = ran.clone();
            move |_: &mut Reader, _: &mut Window, _: &mut Context<Reader>| {
                ran.borrow_mut().push(label)
            }
        };
        r.run_for_pane(r.pane.id(), window, cx, log("active"));
        assert_eq!(*ran.borrow(), ["active"], "the active pane runs it now");
        let closed = reader_panes::PaneId::next();
        r.run_for_pane(closed, window, cx, log("closed"));
        let ran_cx = ran.clone();
        r.run_for_pane_without_window(closed, cx, move |_, _| ran_cx.borrow_mut().push("closed"));
        assert_eq!(
            *ran.borrow(),
            ["active"],
            "a closed pane takes its results with it"
        );
        assert_eq!(
            super::DROPPED_FOR_CLOSED_PANE.with(|dropped| dropped.borrow().clone()),
            vec![closed, closed],
            "both closed-pane completions were seen at the boundary"
        );
        assert!(
            r.pane.deferred.is_empty(),
            "nothing is kept for the active pane"
        );

        // Work kept while the pane was inactive replays in order on
        // activation and still checks its own ticket.
        let stale = r.pane.navigation_ticket();
        r.pane.navigation.generation = r.pane.navigation.generation.wrapping_add(1);
        let fresh = r.pane.navigation_ticket();
        for (ticket, label) in [(stale, "stale"), (fresh, "fresh")] {
            let ran = ran.clone();
            r.pane.deferred.push(Box::new(move |this, _, _| {
                if this.pane.holds_navigation(ticket) {
                    ran.borrow_mut().push(label);
                }
            }));
        }
        r.resume_active_pane(window, cx);
        assert_eq!(
            *ran.borrow(),
            ["active", "fresh"],
            "a stale replay changes nothing"
        );
        assert!(r.pane.deferred.is_empty());
        assert_eq!(r.pane.refused, vec![stale]);
    });
}

#[gpui::test]
fn pane773_quit_protection_visits_every_pane(cx: &mut gpui::TestAppContext) {
    let (reader, visual, _dir) = fixture(cx);
    let a = reader.read_with(visual, |r, _| r.vault_root.join("A.md"));
    reader.update_in(visual, |r, window, cx| {
        window.activate_window();
        r.toggle_source(window, cx);
        let input = r
            .pane
            .editing
            .as_ref()
            .expect("Edit opened")
            .input()
            .clone();
        input.update(cx, |input, cx| {
            input.replace_text_in_range(Some(0..0), "unsaved ", window, cx)
        });
    });
    visual.run_until_parked();
    reader.update(visual, |r, cx| {
        let pane = r.pane.id();
        let visited = r.for_each_pane(cx, |r, cx| (r.pane.id(), r.save_source_for_close(cx)));
        assert_eq!(
            visited,
            vec![(pane, true)],
            "each pane once, with its own result"
        );
    });
    visual.run_until_parked();
    assert!(
        std::fs::read_to_string(&a).unwrap().starts_with("unsaved "),
        "the pane's edit is saved through the per-pane quit path"
    );
}
