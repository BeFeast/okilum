//! Stage 2 of #773: late document work stays bound to its pane and generation.
//! One pane is visible; replacing its note and closing it (a fresh surface in
//! its place) are the two ways a completion becomes stale.
use super::PaneExit;
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

// Stage 3: one editing owner per file, and pane exits through the existing
// save/protect/conflict lifecycle. One pane is visible; the hooks are driven
// directly.

fn note_path(reader: &Entity<Reader>, visual: &mut gpui::VisualTestContext, rel: &str) -> PathBuf {
    reader.read_with(visual, |r, _| r.vault_root.join(rel))
}

fn start_editing(reader: &Entity<Reader>, visual: &mut gpui::VisualTestContext, typed: &str) {
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
            input.replace_text_in_range(Some(0..0), typed, window, cx)
        });
    });
    visual.run_until_parked();
}

#[gpui::test]
fn pane773_one_editing_owner_per_file(cx: &mut gpui::TestAppContext) {
    let (reader, visual, _dir) = fixture(cx);
    let a = note_path(&reader, visual, "A.md");
    let b = note_path(&reader, visual, "B.md");
    let other = reader_panes::PaneId::next();
    reader.read_with(visual, |r, _| {
        assert_eq!(r.editor_owner(&a), None, "Read mode owns nothing");
        assert_eq!(
            r.claim_editor(other, &a),
            Ok(()),
            "positive control: free file"
        );
    });
    start_editing(&reader, visual, "mine ");
    reader.read_with(visual, |r, _| {
        let pane = r.pane.id();
        assert_eq!(r.editor_owner(&a), Some(pane));
        assert_eq!(
            r.claim_editor(pane, &a),
            Ok(()),
            "the owner may keep editing"
        );
        assert_eq!(
            r.claim_editor(other, &a),
            Err(pane),
            "another pane is refused"
        );
        assert_eq!(
            r.claim_editor(other, &b),
            Ok(()),
            "a different file is free"
        );
    });
    reader.update(visual, |r, cx| assert!(r.leave_source(cx)));
    visual.run_until_parked();
    reader.read_with(visual, |r, _| {
        assert_eq!(
            r.editor_owner(&a),
            None,
            "leaving the editor releases ownership"
        );
        assert_eq!(r.claim_editor(other, &a), Ok(()));
    });
}

#[gpui::test]
fn pane773_last_pane_close_and_unsplit_keep_editor_and_draft(cx: &mut gpui::TestAppContext) {
    let (reader, visual, _dir) = fixture(cx);
    let a = note_path(&reader, visual, "A.md");
    let saved = std::fs::read_to_string(&a).unwrap();
    start_editing(&reader, visual, "draft ");
    reader.update_in(visual, |r, window, cx| {
        let pane = r.pane.id();
        assert_eq!(
            r.close_pane(reader_panes::PaneId::next(), window, cx),
            Err(PaneExit::Missing)
        );
        assert_eq!(r.close_pane(pane, window, cx), Err(PaneExit::LastPane));
        assert_eq!(r.unsplit(window, cx), Err(PaneExit::LastPane));
        let editing = r.pane.editing.as_ref().expect("editor kept");
        assert!(editing
            .input()
            .read(cx)
            .value()
            .to_string()
            .starts_with("draft "));
        assert_eq!(r.editor_owner(&a), Some(pane));
    });
    assert_eq!(
        std::fs::read_to_string(&a).unwrap(),
        saved,
        "a refused close saves nothing"
    );
}

#[gpui::test]
fn pane773_pane_exit_saves_or_refuses_through_the_lifecycle(cx: &mut gpui::TestAppContext) {
    let (reader, visual, _dir) = fixture(cx);
    let a = note_path(&reader, visual, "A.md");
    // Conflict: the file changed on disk under a dirty editor. The guard
    // refuses and keeps the pane, the editor and the draft.
    start_editing(&reader, visual, "mine ");
    std::fs::write(&a, "external\n").unwrap();
    reader.update(visual, |r, cx| {
        let pane = r.pane.id();
        assert_eq!(
            r.guard_pane_exit(reader_panes::PaneId::next(), cx),
            Err(PaneExit::Missing)
        );
        assert_eq!(r.guard_pane_exit(pane, cx), Err(PaneExit::Refused));
        let editing = r.pane.editing.as_ref().expect("editor kept on refusal");
        assert!(editing
            .input()
            .read(cx)
            .value()
            .to_string()
            .starts_with("mine "));
    });
    visual.run_until_parked();
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "external\n");
    // Resolved to the disk version and edited again: the guard saves and
    // releases the editor.
    reader.update_in(visual, |r, window, cx| {
        r.resolve_source(true, window, cx);
        let input = r.pane.editing.as_ref().unwrap().input().clone();
        input.update(cx, |input, cx| {
            input.replace_text_in_range(Some(0..0), "saved-", window, cx)
        });
    });
    visual.run_until_parked();
    reader.update(visual, |r, cx| {
        let pane = r.pane.id();
        assert_eq!(r.guard_pane_exit(pane, cx), Ok(()));
        assert!(r.pane.editing.is_none());
        assert_eq!(r.editor_owner(&a), None);
    });
    visual.run_until_parked();
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "saved-external\n");
}

// Stage 4: the second pane. `Reader::pane` is always the active surface;
// `Reader::split` holds the inactive one beside it.

fn rel_of(surface: &super::Surface) -> &str {
    surface.current_rel.as_str()
}

fn other_rel(r: &Reader) -> String {
    r.split
        .as_ref()
        .map(|split| rel_of(&split.other).to_owned())
        .unwrap_or_default()
}

#[gpui::test]
fn pane773_split_opens_beside_and_focus_keeps_each_pane(cx: &mut gpui::TestAppContext) {
    let (reader, visual, _dir) = fixture(cx);
    assert!(
        visual.debug_bounds("reader-inactive-pane").is_none(),
        "one pane at first"
    );
    reader.update_in(visual, |r, window, cx| {
        r.open_beside("B.md", None, window, cx)
    });
    settle(visual);
    reader.read_with(visual, |r, _| {
        assert_eq!(
            r.pane.current_rel, "B.md",
            "the new pane is active and shows B"
        );
        assert_eq!(other_rel(r), "A.md", "the first pane keeps A");
        assert!(
            !r.split.as_ref().unwrap().active_left,
            "B opened on the right"
        );
        assert_eq!(r.pane.navigation.history, ["B.md"]);
    });
    assert!(
        visual.debug_bounds("reader-inactive-pane").is_some(),
        "both panes drawn"
    );
    reader.update_in(visual, |r, window, cx| {
        r.open_note("C.md", None, window, cx)
    });
    settle(visual);
    reader.update_in(visual, |r, window, cx| r.focus_other_pane(window, cx));
    settle(visual);
    reader.read_with(visual, |r, _| {
        assert_eq!(r.pane.current_rel, "A.md", "focus brings A back as it was");
        assert_eq!(
            r.pane.navigation.history,
            ["A.md"],
            "A's history is its own"
        );
        assert_eq!(other_rel(r), "C.md");
        let other = &r.split.as_ref().unwrap().other;
        assert_eq!(
            other.navigation.history,
            ["B.md", "C.md"],
            "and the other pane keeps its own"
        );
    });
}

#[gpui::test]
fn pane773_click_on_the_inactive_pane_only_focuses_it(cx: &mut gpui::TestAppContext) {
    let (reader, visual, _dir) = fixture(cx);
    reader.update_in(visual, |r, window, cx| {
        r.open_beside("B.md", None, window, cx)
    });
    settle(visual);
    let layer = visual
        .debug_bounds("reader-inactive-pane-focus")
        .expect("the inactive pane's focus layer is drawn");
    visual.simulate_click(layer.center(), Modifiers::default());
    settle(visual);
    reader.read_with(visual, |r, _| {
        assert_eq!(r.pane.current_rel, "A.md", "the clicked pane is active");
        assert_eq!(other_rel(r), "B.md", "and nothing else changed");
    });
}

#[gpui::test]
fn pane773_modified_click_on_a_note_link_opens_it_beside(cx: &mut gpui::TestAppContext) {
    let (reader, visual, dir) = fixture(cx);
    std::fs::write(dir.path().join("notes/C.md"), "# C\n\nSee [[B]].\n").unwrap();
    reader.update_in(visual, |r, window, cx| {
        r.open_note("C.md", None, window, cx)
    });
    settle(visual);
    let url = reader.read_with(visual, |r, _| {
        r.pane
            .link_identities
            .iter()
            .find(|link| link.target == "B")
            .expect("positive control: the link is prepared")
            .url
            .clone()
    });
    assert_eq!(
        super::Destination::of_click(gpui::Modifiers::secondary_key()),
        super::Destination::Beside
    );
    visual.update(|window, cx| {
        reader_link_navigation::handle_link_to(
            &reader.downgrade(),
            &url,
            super::Destination::Beside,
            window,
            cx,
        )
    });
    settle(visual);
    reader.read_with(visual, |r, _| {
        assert_eq!(
            r.pane.current_rel, "B.md",
            "the link opened in the new pane"
        );
        assert_eq!(
            other_rel(r),
            "C.md",
            "the pane the link was in keeps its note"
        );
    });
    // An ordinary click still navigates in place.
    visual.update(|window, cx| {
        reader_link_navigation::handle_link(&reader.downgrade(), &url, window, cx)
    });
    settle(visual);
    reader.read_with(visual, |r, _| assert_eq!(other_rel(r), "C.md"));
}

#[gpui::test]
fn pane773_same_note_can_be_edited_in_one_pane_only(cx: &mut gpui::TestAppContext) {
    let (reader, visual, _dir) = fixture(cx);
    reader.update_in(visual, |r, window, cx| r.toggle_split(window, cx));
    settle(visual);
    reader.read_with(visual, |r, _| {
        assert_eq!(r.pane.current_rel, "A.md");
        assert_eq!(other_rel(r), "A.md", "the split shows A in both panes");
    });
    reader.update_in(visual, |r, window, cx| r.focus_other_pane(window, cx));
    settle(visual);
    start_editing(&reader, visual, "mine ");
    reader.update_in(visual, |r, window, cx| {
        r.focus_other_pane(window, cx);
        r.toggle_source(window, cx);
        assert!(r.pane.editing.is_none(), "the second pane is refused");
        assert_eq!(r.link_notice.as_deref(), Some(super::EDITED_ELSEWHERE));
        let owner = r.split.as_ref().unwrap().other.editing.as_ref();
        assert!(owner.is_some(), "the owner keeps its editor");
    });
}

#[gpui::test]
fn pane773_unsplit_runs_the_inactive_panes_guard(cx: &mut gpui::TestAppContext) {
    let (reader, visual, _dir) = fixture(cx);
    let a = note_path(&reader, visual, "A.md");
    start_editing(&reader, visual, "mine ");
    reader.update_in(visual, |r, window, cx| {
        r.open_beside("B.md", None, window, cx)
    });
    settle(visual);
    std::fs::write(&a, "external\n").unwrap();
    reader.update_in(visual, |r, window, cx| {
        assert_eq!(
            r.unsplit(window, cx),
            Err(PaneExit::Refused),
            "A's edit conflicts"
        );
        assert!(r.split.is_some(), "the layout stays split");
        let editing = r
            .split
            .as_ref()
            .unwrap()
            .other
            .editing
            .as_ref()
            .expect("A keeps its editor");
        assert!(editing
            .input()
            .read(cx)
            .value()
            .to_string()
            .starts_with("mine "));
        assert_eq!(r.pane.current_rel, "B.md", "the active pane is untouched");
    });
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "external\n");
}

#[gpui::test]
fn pane773_unsplit_keeps_the_active_pane(cx: &mut gpui::TestAppContext) {
    let (reader, visual, _dir) = fixture(cx);
    reader.update_in(visual, |r, window, cx| {
        r.open_beside("B.md", None, window, cx)
    });
    settle(visual);
    reader.update_in(visual, |r, window, cx| {
        let b = r.pane.id();
        assert_eq!(r.unsplit(window, cx), Ok(()));
        assert!(r.split.is_none());
        assert_eq!(r.pane.id(), b, "the active pane stays");
        assert_eq!(r.pane.current_rel, "B.md");
    });
    settle(visual);
    assert!(visual.debug_bounds("reader-inactive-pane").is_none());
}

#[gpui::test]
fn pane773_quit_protection_saves_the_inactive_pane_too(cx: &mut gpui::TestAppContext) {
    let (reader, visual, _dir) = fixture(cx);
    let a = note_path(&reader, visual, "A.md");
    start_editing(&reader, visual, "unsaved ");
    reader.update_in(visual, |r, window, cx| {
        r.open_beside("B.md", None, window, cx)
    });
    settle(visual);
    reader.update(visual, |r, cx| {
        let results = r.for_each_pane(cx, |r, cx| {
            (r.pane.current_rel.clone(), r.save_source_for_close(cx))
        });
        assert_eq!(results.len(), 2, "both panes are visited");
        assert!(results.iter().all(|(_, saved)| *saved));
        assert_eq!(r.pane.current_rel, "B.md", "order restored after the visit");
    });
    settle(visual);
    assert!(std::fs::read_to_string(&a).unwrap().starts_with("unsaved "));
}

#[gpui::test]
fn pane773_a_pane_loading_in_the_background_publishes_on_activation(cx: &mut gpui::TestAppContext) {
    let (reader, visual, _dir) = fixture(cx);
    let (release, hold) = async_channel::bounded(1);
    reader.update_in(visual, |r, window, cx| {
        r.open_beside("", None, window, cx);
        r.pane.preparation_hold = Some(hold);
        r.open_note("B.md", None, window, cx);
        // Back to A while B is still preparing.
        r.focus_other_pane(window, cx);
    });
    settle(visual);
    release.try_send(()).unwrap();
    settle(visual);
    reader.read_with(visual, |r, _| {
        assert_eq!(
            r.pane.current_rel, "A.md",
            "the active pane was never touched"
        );
        let other = &r.split.as_ref().unwrap().other;
        assert_eq!(rel_of(other), "", "B waits while its pane is inactive");
        assert!(
            !other.deferred.is_empty(),
            "positive control: the result was kept"
        );
    });
    reader.update_in(visual, |r, window, cx| r.focus_other_pane(window, cx));
    settle(visual);
    reader.read_with(visual, |r, _| {
        assert_eq!(
            r.pane.current_rel, "B.md",
            "B publishes when its pane is active"
        );
        assert_eq!(other_rel(r), "A.md");
    });
}
