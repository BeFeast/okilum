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
        assert_eq!(
            r.pane.refused,
            vec![stale],
            "the late completion was observed"
        );
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
