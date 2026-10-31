use super::*;
use ::core::prelude::v1::test;
use std::{cell::Cell, rc::Rc};

fn message(position: u64) -> TimelineItem {
    serde_json::from_value(serde_json::json!({
        "position": position, "itemId": format!("item-{position}"), "messageId": format!("message-{position}"),
        "type": if position.is_multiple_of(2) { "user_message" } else { "assistant_message" },
        "status": "completed", "text": format!("Message {position}: **retained history** with a second line.\n\nText 🧠"), "updatedAt": "now"
    })).unwrap()
}
fn fixture(
    cx: &mut TestAppContext,
    count: u64,
) -> (
    Entity<ProjectView>,
    &mut VisualTestContext,
    tempfile::TempDir,
) {
    cx.update(crate::test_init);
    let temp = tempfile::tempdir().unwrap();
    let (root, visual) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| {
            let mut view = super::super::fixture::view(
                temp.path().to_owned(),
                Arc::new(super::super::fixture::Fake::default()),
                window,
                cx,
            );
            view.active = false;
            let chat = view.chat.as_mut().unwrap();
            chat.project_id = Some("fixture-managers".into());
            chat.items
                .extend((0..count).map(|position| (position, message(position))));
            view.identity = Some(ServerIdentity {
                environment_id: "fixture".into(),
                label: "Fixture".into(),
            });
            view.feed.update(cx, |feed, cx| {
                feed.apply(chat.items.values(), Some(&[]), cx)
            });
            view
        });
        Root::new(entity, window, cx)
    });
    visual.simulate_resize(size(px(760.), px(900.)));
    visual.run_until_parked();
    let entity = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<ProjectView>().unwrap()
    });
    (entity, visual, temp)
}
fn counters(view: &Entity<ProjectView>, visual: &VisualTestContext) -> (usize, usize) {
    view.read_with(visual, |view, cx| {
        let feed = view.feed.read(cx);
        (feed.row_calls.get(), feed.render_calls.get())
    })
}

#[gpui::test]
fn long_thread_scroll_and_streaming_stay_within_the_row_render_budget(cx: &mut TestAppContext) {
    let (view, visual, _temp) = fixture(cx, 5_000);
    let (initial_rows, _) = counters(&view, visual);
    assert!(
        initial_rows > 0,
        "positive control: real message rows were rendered"
    );
    assert!(initial_rows <= 512, "FEED1787: eager initialization exceeded 512 row renders across initial layouts: {initial_rows}");
    view.read_with(visual, |view, cx| view.feed.read(cx).row_calls.set(0));
    visual.simulate_resize(size(px(784.), px(900.)));
    visual.run_until_parked();
    let (rows, _) = counters(&view, visual);
    assert!(
        rows > 0 && rows <= 128,
        "FEED1787: redraw exceeded 128 rendered rows: {rows}"
    );
    assert!(
        visual.debug_bounds("project-message-4999").is_some(),
        "initial tail is visible"
    );
    let feed = view.read_with(visual, |view, _| view.feed.clone());
    feed.update(visual, |feed, cx| {
        assert!(feed.scroller.read(cx).is_following_tail());
        feed.row_calls.set(0);
        feed.scroller.update(cx, |state, cx| {
            assert!(state.scroll_to_item(100, cx));
        });
    });
    visual.run_until_parked();
    let (rows, _) = counters(&view, visual);
    assert!(rows > 0 && rows <= 128, "scroll budget: {rows}");
    feed.read_with(visual, |feed, cx| {
        assert!(!feed.scroller.read(cx).is_following_tail())
    });
    assert!(visual.debug_bounds("project-message-4999").is_none());
    view.update(visual, |view, cx| {
        let item = message(5_000);
        view.chat
            .as_mut()
            .unwrap()
            .items
            .insert(5_000, item.clone());
        view.feed.update(cx, |feed, cx| {
            feed.row_calls.set(0);
            feed.apply(std::iter::once(&item), None, cx);
        });
    });
    visual.run_until_parked();
    feed.read_with(visual, |feed, cx| {
        assert!(
            !feed.scroller.read(cx).is_following_tail(),
            "append preserves reader's scroll choice"
        )
    });
    feed.update(visual, |feed, cx| {
        feed.scroller
            .update(cx, |state, cx| state.scroll_to_end(cx))
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("project-message-5000").is_some());
    view.update(visual, |view, cx| {
        let item = view.chat.as_mut().unwrap().items.get_mut(&5_000).unwrap();
        item.text = Some("A growing streaming reply\n\n".repeat(20));
        view.feed.update(cx, |feed, cx| {
            feed.row_calls.set(0);
            feed.apply(std::iter::once(&*item), None, cx);
        });
    });
    visual.run_until_parked();
    feed.read_with(visual, |feed, cx| {
        assert!(
            feed.scroller.read(cx).is_following_tail(),
            "streaming row growth follows the live edge"
        );
        assert!(
            feed.row_calls.get() > 0 && feed.row_calls.get() <= 128,
            "streaming render budget: {}",
            feed.row_calls.get()
        );
    });
}

#[gpui::test]
fn composer_keystrokes_do_not_render_or_lay_out_history(cx: &mut TestAppContext) {
    // The scrollbar uses wall-clock Instant, independently of the test clock.
    // Disable decorative motion here to isolate input invalidation; the long
    // thread/scroll/stream budget test above exercises the normal motion policy.
    cx.update(|cx| cx.set_reduce_motion(true));
    let (view, visual, _temp) = fixture(cx, 5_000);
    view.update_in(visual, |view, window, cx| {
        view.compose.read(cx).focus_handle(cx).focus(window, cx)
    });
    visual.run_until_parked();
    // Enter keyboard modality before measuring input: GPUI refreshes the whole
    // window once when switching from mouse to keyboard focus styling.
    visual.simulate_keystrokes("left");
    visual.run_until_parked();
    let signals = Rc::new([Cell::new(0), Cell::new(0), Cell::new(0)]);
    let _observer: Entity<()> = visual.update(|_, cx| {
        let feed = view.read(cx).feed.clone();
        let composer = view.read(cx).composer.clone();
        cx.new(|cx| {
            let counts = signals.clone();
            cx.observe(&view, move |_, _, _| counts[0].set(counts[0].get() + 1))
                .detach();
            let counts = signals.clone();
            cx.observe(&feed, move |_, _, _| counts[1].set(counts[1].get() + 1))
                .detach();
            let counts = signals.clone();
            cx.observe(&composer, move |_, _, _| counts[2].set(counts[2].get() + 1))
                .detach();
        })
    });
    let before = counters(&view, visual);
    for text in ["alpha", "beta", "gamma"] {
        visual.simulate_input(text);
        visual.run_until_parked();
    }
    view.read_with(visual, |view, cx| {
        assert_eq!(view.compose.read(cx).value().as_ref(), "alphabetagamma")
    });
    assert_eq!(
        counters(&view, visual),
        before,
        "COMPOSER1787: typing invalidated the history entity"
    );
    assert_eq!(signals[0].get(), 0, "typing must not notify the panel");
    assert_eq!(signals[1].get(), 0, "typing must not notify the feed");
    assert!(signals[2].get() > 0, "positive control: composer changed");
    assert!(visual.debug_bounds("project-send").is_some());
}

#[gpui::test]
fn unchanged_poll_notifies_neither_panel_nor_feed_and_changed_reply_is_a_positive_control(
    cx: &mut TestAppContext,
) {
    cx.update(crate::test_init);
    let temp = tempfile::tempdir().unwrap();
    let gateway = Arc::new(super::super::fixture::Fake::default());
    let (root, visual) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| {
            super::super::fixture::view(temp.path().to_owned(), gateway.clone(), window, cx)
        });
        Root::new(entity, window, cx)
    });
    visual.run_until_parked();
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<ProjectView>().unwrap()
    });
    let notified = Rc::new(Cell::new(0));
    let counter = notified.clone();
    let _observer: Entity<()> = visual.update(|_, cx| {
        cx.new(|cx| {
            cx.observe(&view, move |_, _, _| counter.set(counter.get() + 1))
                .detach();
        })
    });
    let before = counters(&view, visual);
    let reads = gateway.reads.lock().unwrap().len();
    visual.executor().advance_clock(Duration::from_secs(16));
    visual.run_until_parked();
    assert!(
        gateway.reads.lock().unwrap().len() > reads,
        "positive control: the idle poll actually ran"
    );
    assert_eq!(
        notified.get(),
        0,
        "POLL1787: unchanged poll notified the panel"
    );
    assert_eq!(counters(&view, visual), before);
    *gateway.grown.lock().unwrap() = true;
    visual.executor().advance_clock(Duration::from_secs(16));
    visual.run_until_parked();
    assert!(
        notified.get() > 0,
        "positive control: a changed reply notifies"
    );
    assert!(
        counters(&view, visual).0 > before.0,
        "a changed reply updates the visible feed"
    );
}

#[gpui::test]
fn projected_receipts_hide_retained_duplicates_without_changing_retry_identity(
    cx: &mut TestAppContext,
) {
    let (view, visual, _temp) = fixture(cx, 20);
    let mut queued = MessageRecord {
        created_at_millis: 1,
        message: okilum_t3::OutgoingMessage::queue("mcp:fixture-manager", "queued"),
        receipt: None,
    };
    let retry = queued.clone();
    queued.receipt = Some(okilum_t3::SendReceipt {
        thread_id: "mcp:fixture-manager".into(),
        message_id: "message-19".into(),
        run_id: "run".into(),
        status: "queued".into(),
        delivery: "queued".into(),
    });
    view.update(visual, |view, cx| {
        view.records = vec![queued, retry.clone()];
        view.feed.update(cx, |feed, cx| {
            feed.apply(std::iter::empty(), Some(&view.records), cx)
        });
    });
    visual.run_until_parked();
    view.read_with(visual, |view, cx| {
        let feed = view.feed.read(cx);
        assert_eq!(feed.rows.len(), 21);
        assert_eq!(feed.rows.last(), Some(&Row::Retained(1)));
        assert_eq!(
            feed.records[1].message.client_request_id,
            retry.message.client_request_id
        );
    });
    assert!(visual.debug_bounds("project-retry-1").is_some());
}

#[gpui::test]
fn activity_rows_render_tools_and_keep_reasoning_collapsed(cx: &mut TestAppContext) {
    let (view, visual, _temp) = fixture(cx, 0);
    let mut reasoning = message(1);
    reasoning.kind = "reasoning".into();
    reasoning.text = Some("Reasoning stays collapsed".into());
    let mut tool = message(2);
    tool.kind = "command_execution".into();
    tool.title = Some("Command · completed".into());
    tool.text = Some("cargo test".into());
    view.update(visual, |view, cx| {
        view.feed.update(cx, |feed, cx| {
            feed.apply([&reasoning, &tool].into_iter(), None, cx)
        })
    });
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("project-tool-2").is_some(),
        "ACTIVITY1787: message-only filter hid the command row"
    );
    assert!(
        visual.debug_bounds("project-reasoning-toggle-1").is_some(),
        "ACTIVITY1787: reasoning disclosure is absent"
    );
}

#[gpui::test]
fn tool_disclosure_keeps_latest_detail_without_repainting_hidden_output(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let (view, visual, _temp) = fixture(cx, 0);
    let mut tool = message(1);
    tool.kind = "command_execution".into();
    tool.title = Some("Command · running".into());
    tool.text = Some(format!(
        "cargo check\n{}",
        "output **detail**\n".repeat(2_000)
    ));
    let feed = view.read_with(visual, |view, _| view.feed.clone());
    feed.update(visual, |feed, cx| {
        feed.apply(std::iter::once(&tool), None, cx)
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("project-tool-preview-1").is_some());
    assert!(visual.debug_bounds("project-tool-detail-1").is_none());
    let notified = Rc::new(Cell::new(0));
    let _observer: Entity<()> = visual.update(|_, cx| {
        cx.new(|cx| {
            let counter = notified.clone();
            cx.observe(&feed, move |_, _, _| counter.set(counter.get() + 1))
                .detach();
        })
    });
    let before = counters(&view, visual);
    tool.text = Some("cargo check\n**Latest retained result**".into());
    feed.update(visual, |feed, cx| {
        feed.apply(std::iter::once(&tool), None, cx)
    });
    visual.run_until_parked();
    assert_eq!(
        notified.get(),
        0,
        "hidden output must not invalidate the feed"
    );
    assert_eq!(counters(&view, visual), before);
    feed.read_with(visual, |feed, _| {
        assert_eq!(
            feed.messages[&1].text.as_ref(),
            tool.text.as_deref().unwrap()
        );
    });
    let toggle = visual.debug_bounds("project-tool-toggle-1").unwrap();
    visual.simulate_click(toggle.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("project-tool-detail-1").is_some(),
        "positive control: a real click expands the retained detail"
    );
    assert!(visual.debug_bounds("project-tool-preview-1").is_none());
    assert!(notified.get() > 0);
    assert!(counters(&view, visual).0 > before.0);

    // Expanded output and changed status must still refresh visibly.
    let notified_before = notified.get();
    tool.text = Some("cargo check\nFinished".into());
    tool.title = Some("Command · completed".into());
    feed.update(visual, |feed, cx| {
        feed.apply(std::iter::once(&tool), None, cx)
    });
    visual.run_until_parked();
    assert!(notified.get() > notified_before);
    let toggle = visual.debug_bounds("project-tool-toggle-1").unwrap();
    visual.simulate_click(toggle.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(visual.debug_bounds("project-tool-detail-1").is_none());
    assert!(visual.debug_bounds("project-tool-preview-1").is_some());
    let before_preview = notified.get();
    tool.text = Some("cargo check --all-targets\nFinished".into());
    feed.update(visual, |feed, cx| {
        feed.apply(std::iter::once(&tool), None, cx)
    });
    visual.run_until_parked();
    assert!(
        notified.get() > before_preview,
        "positive control: a changed collapsed preview refreshes"
    );
}

#[gpui::test]
fn questions_and_failures_stay_visible_and_reasoning_can_expand(cx: &mut TestAppContext) {
    let (view, visual, _temp) = fixture(cx, 0);
    let mut reasoning = message(1);
    reasoning.kind = "reasoning".into();
    reasoning.text = Some("Retained reasoning".into());
    let mut question = message(2);
    question.kind = "user_input_request".into();
    question.text = Some("Which branch?".into());
    let mut error = message(3);
    error.kind = "error".into();
    error.text = Some("The request failed".into());
    view.update(visual, |view, cx| {
        view.feed.update(cx, |feed, cx| {
            feed.apply([&reasoning, &question, &error].into_iter(), None, cx)
        })
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("project-tool-detail-2").is_some());
    assert!(visual.debug_bounds("project-tool-detail-3").is_some());
    assert!(visual.debug_bounds("project-tool-toggle-2").is_none());
    assert!(visual.debug_bounds("project-tool-toggle-3").is_none());
    assert!(visual.debug_bounds("project-reasoning-detail-1").is_none());
    let toggle = visual.debug_bounds("project-reasoning-toggle-1").unwrap();
    visual.simulate_click(toggle.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(visual.debug_bounds("project-reasoning-detail-1").is_some());
    // The visible Open in T3 action still routes to the bound thread.
    let open = visual.debug_bounds("project-open-t3").unwrap();
    visual.simulate_click(open.center(), Modifiers::default());
    let expected = view.read_with(visual, |view, _| {
        view.identity
            .as_ref()
            .unwrap()
            .thread_url(&view.chat.as_ref().unwrap().thread_id)
    });
    assert_eq!(visual.opened_url().as_deref(), Some(expected.as_str()));
}

#[gpui::test]
fn native_activity_history_remains_virtualized_and_hidden_updates_are_quiet(
    cx: &mut TestAppContext,
) {
    use okilum_t3::projection::TurnRow;
    cx.update(|cx| cx.set_reduce_motion(true));
    let (view, visual, _temp) = fixture(cx, 0);
    let feed = view.read_with(visual, |view, _| view.feed.clone());
    let rows: Vec<_> = (0..5_000).map(|index| Arc::new(TurnRow {
        key: RowKey { thread: "native-fixture".into(), item: format!("tool-{index}") },
        local: true,
        item: Arc::new(serde_json::json!({"type": "command_execution", "status": "completed", "input": "x".repeat(240)})),
    })).collect();
    feed.update(visual, |feed, cx| {
        feed.row_calls.set(0);
        feed.apply_native(
            NativeChanges {
                order: Some(rows.iter().map(|row| row.key.clone()).collect()),
                rows: rows.clone(),
                reset: true,
                ..Default::default()
            },
            &[],
            cx,
        );
    });
    visual.run_until_parked();
    let before = counters(&view, visual);
    assert!(
        before.0 > 0 && before.0 <= 512,
        "positive control: native activity rows rendered within budget: {}",
        before.0
    );
    assert!(visual.debug_bounds("project-tool-toggle-5000").is_some());
    assert!(visual.debug_bounds("project-tool-detail-5000").is_none());
    let replacement = Arc::new(TurnRow {
        key: rows.last().unwrap().key.clone(),
        local: true,
        item: Arc::new(
            serde_json::json!({"type": "command_execution", "status": "completed", "input": format!("{}latest hidden detail", "x".repeat(121))}),
        ),
    });
    feed.update(visual, |feed, cx| {
        feed.apply_native(
            NativeChanges {
                rows: vec![replacement],
                ..Default::default()
            },
            &[],
            cx,
        )
    });
    visual.run_until_parked();
    assert_eq!(
        counters(&view, visual),
        before,
        "unchanged native preview must not lay out history"
    );
    feed.read_with(visual, |feed, _| {
        assert!(feed.messages[&5000].text.ends_with("latest hidden detail"))
    });
}
