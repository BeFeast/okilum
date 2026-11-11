use super::*;
use ::core::prelude::v1::test;
use okilum_t3::projection::{Changes, RowKey, TurnRow};
use std::{cell::Cell, rc::Rc};

const THREAD: &str = "mcp:fixture-manager";

fn request(id: &str, kind: &str) -> Request {
    let mut item = json!({"id":format!("row-{id}"),"threadId":THREAD,"requestId":id,
        "ordinal":1001,"status":"pending","type":kind,"prompt":format!("Approve {id}")});
    if kind == "user_input_request" {
        item["questions"] = json!([{"id":"q","question":"Choose or write an answer","multiSelect":true,
            "options":[{"label":"First","value":" exact "},{"label":"Second","value":"β"}]}]);
    }
    Request {
        id: id.into(),
        item,
        answerable: true,
        dismissible: kind == "user_input_request",
        response_mode: Some("live".into()),
        approval_detail: None,
        reason: None,
    }
}

struct Host {
    project: Entity<ProjectView>,
    switch_on_release: Rc<Cell<bool>>,
}
impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let armed = self.switch_on_release.clone();
        let project = self.project.clone();
        div()
            .size_full()
            .capture_any_mouse_up(move |_, window, cx| {
                if !armed.replace(false) {
                    return;
                }
                let project = project.clone();
                // Runs after the real button click but before its deferred
                // dispatch, reproducing an intervening conversation switch.
                window.defer(cx, move |window, cx| {
                    project.update(cx, |view, cx| {
                        view.chat = Some(Conversation::new("replacement"));
                        view.binding.as_mut().unwrap().manager.thread_id =
                            Some("replacement".into());
                        view.sync_native_action_surface(cx);
                        let mut next = request("same", "approval_request");
                        next.item["threadId"] = json!("replacement");
                        view.native.actions.update(cx, |actions, cx| {
                            actions.set_state(
                                State {
                                    ready: true,
                                    requests: vec![next],
                                    ..Default::default()
                                },
                                window,
                                cx,
                            );
                        });
                        cx.notify();
                    });
                });
            })
            .child(self.project.clone())
    }
}

fn fixture(
    cx: &mut TestAppContext,
    history: u64,
) -> (
    Entity<ProjectView>,
    &mut VisualTestContext,
    tempfile::TempDir,
    Rc<Cell<bool>>,
) {
    cx.update(|cx| {
        crate::test_init(cx);
        cx.set_reduce_motion(true);
    });
    let temp = tempfile::tempdir().unwrap();
    let armed = Rc::new(Cell::new(false));
    let mut project = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = super::super::super::fixture::view(
                temp.path().to_owned(), Arc::new(super::super::super::fixture::Fake::default()), window, cx,
            );
            view.active = false;
            super::tests::inert_native(&mut view);
            view.sync_native_action_surface(cx);
            let rows: Vec<_> = (0..history).map(|n| Arc::new(TurnRow {
                key: RowKey { thread: THREAD.into(), item: format!("history-{n}") }, local: true,
                item: Arc::new(json!({"id":format!("history-{n}"),"threadId":THREAD,"type":"assistant_message",
                    "status":"completed","streaming":false,"text":format!("Retained history {n}")})),
            })).collect();
            view.feed.update(cx, |feed, cx| feed.apply_native(Changes {
                order: Some(rows.iter().map(|row| row.key.clone()).collect()), rows, ..Default::default()
            }, &[], cx));
            view
        });
        project = Some(view.clone());
        let host = cx.new(|_| Host { project: view, switch_on_release: armed.clone() });
        Root::new(host, window, cx)
    });
    visual.simulate_resize(size(px(760.), px(1000.)));
    visual.run_until_parked();
    (project.unwrap(), visual, temp, armed)
}

fn show(
    view: &Entity<ProjectView>,
    visual: &mut VisualTestContext,
    history: u64,
    requests: Vec<Request>,
) {
    view.update_in(visual, |view, window, cx| {
        view.sync_native_action_surface(cx);
        let rows: Vec<_> = requests
            .iter()
            .map(|request| {
                Arc::new(TurnRow {
                    key: RowKey {
                        thread: view.chat.as_ref().unwrap().thread_id.clone(),
                        item: request.item["id"].as_str().unwrap().into(),
                    },
                    local: true,
                    item: Arc::new(request.item.clone()),
                })
            })
            .collect();
        let mut order: Vec<_> = (0..history)
            .map(|n| RowKey {
                thread: THREAD.into(),
                item: format!("history-{n}"),
            })
            .collect();
        order.extend(rows.iter().map(|row| row.key.clone()));
        view.native.actions.update(cx, |actions, cx| {
            actions.set_state(
                State {
                    ready: true,
                    requests,
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        view.feed.update(cx, |feed, cx| {
            feed.apply_native(
                Changes {
                    rows,
                    order: Some(order),
                    ..Default::default()
                },
                &[],
                cx,
            );
            feed.requests_changed(cx);
        });
        cx.notify();
    });
    visual.run_until_parked();
}

fn bounds(visual: &mut VisualTestContext, selector: &str) -> Option<gpui::Bounds<Pixels>> {
    // GPUI's test-only debug_bounds API requires &'static str and exposes no
    // owned-selector overload. Intern each finite fixture selector once.
    static SELECTORS: std::sync::OnceLock<std::sync::Mutex<HashMap<String, &'static str>>> =
        std::sync::OnceLock::new();
    let selector = {
        let mut selectors = SELECTORS.get_or_init(Default::default).lock().unwrap();
        *selectors
            .entry(selector.to_owned())
            .or_insert_with(|| Box::leak(selector.to_owned().into_boxed_str()))
    };
    visual.debug_bounds(selector)
}

fn click(visual: &mut VisualTestContext, selector: &str) {
    let bounds = bounds(visual, selector)
        .unwrap_or_else(|| panic!("positive control: {selector} is rendered"));
    visual.simulate_click(bounds.center(), Modifiers::default());
    visual.run_until_parked();
}

fn key(visual: &mut VisualTestContext, name: &str) {
    let keystroke = gpui::Keystroke::parse(name).unwrap();
    visual.simulate_event(gpui::KeyDownEvent {
        keystroke: keystroke.clone(),
        is_held: false,
        prefer_character_input: false,
    });
    visual.simulate_event(gpui::KeyUpEvent { keystroke });
    visual.run_until_parked();
}

fn wheel(visual: &mut VisualTestContext, selector: &str, delta: f32) {
    let bounds = bounds(visual, selector).unwrap();
    visual.simulate_event(gpui::ScrollWheelEvent {
        position: bounds.center(),
        delta: gpui::ScrollDelta::Pixels(point(px(0.), px(delta))),
        ..Default::default()
    });
    visual.run_until_parked();
}

fn input(view: &Entity<ProjectView>, visual: &VisualTestContext, id: &str) -> Entity<InputState> {
    view.read_with(visual, |view, cx| {
        view.native.actions.read(cx).inputs[&(id.into(), "q".into())].clone()
    })
}

fn answers(
    view: &Entity<ProjectView>,
    visual: &VisualTestContext,
    id: &str,
) -> BTreeMap<String, Value> {
    view.read_with(visual, |view, cx| {
        match view.native.actions.read(cx).intent(id, cx) {
            Intent::Respond {
                answers: Some(answers),
                ..
            } => answers,
            _ => panic!("question intent"),
        }
    })
}

#[gpui::test]
fn pinned_long_question_preserves_interior_history_and_custom_choice_exclusivity(
    cx: &mut TestAppContext,
) {
    let (view, visual, _temp, _) = fixture(cx, 1000);
    assert!(
        visual.debug_bounds("project-message-1000").is_some(),
        "positive control: history starts at tail"
    );
    wheel(visual, "project-history", 1800.);
    assert!(
        visual.debug_bounds("project-message-1000").is_none(),
        "real wheel left the latest history"
    );
    let logical_before = view.read_with(visual, |view, cx| {
        view.feed
            .read(cx)
            .test_scroll_anchor(cx)
            .expect("positive control: source-keyed interior list offset")
    });
    let history = visual.debug_bounds("project-history").unwrap();
    let positions = view.read_with(visual, |view, cx| {
        view.feed.read(cx).rendered_positions.borrow().clone()
    });
    let (position, before) = positions
        .into_iter()
        .filter_map(|position| {
            let bounds = bounds(visual, &format!("project-message-{position}"))?;
            (bounds.origin.y >= history.origin.y && bounds.bottom() <= history.bottom())
                .then_some((position, bounds.origin.y - history.origin.y))
        })
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
        .expect("positive control: fully visible interior source message");
    let mut question = request("question", "user_input_request");
    question.item["questions"][0]["question"] =
        json!("Long question keeps every detail accessible. ".repeat(70));
    show(&view, visual, 1000, vec![question]);
    let logical_after =
        view.read_with(visual, |view, cx| view.feed.read(cx).test_scroll_anchor(cx));
    diagnostic_error!("request arrival: logical_before={logical_before:?}; logical_after={logical_after:?}; topmost_full_source={position}; pixel_offset={before:?}");
    assert_eq!(
        Some(logical_before.clone()),
        logical_after,
        "request arrival preserves the actual list source key, item index and intra-row offset"
    );
    let panel = visual.debug_bounds("project-native-actions").unwrap();
    let composer = visual.debug_bounds("project-chat-composer").unwrap();
    assert!(panel.size.height <= px(280.));
    assert!(
        panel.top() >= composer.top()
            && panel.bottom() <= visual.debug_bounds("project-composer-input").unwrap().top(),
        "current request is outside history, inside the message surface above text entry"
    );
    let after = bounds(visual, &format!("project-message-{position}"))
        .unwrap()
        .origin
        .y
        - visual.debug_bounds("project-history").unwrap().origin.y;
    assert_eq!(
        before, after,
        "request arrival preserves interior source and pixel offset"
    );
    assert!(
        visual.debug_bounds("project-message-1001").is_none(),
        "answering does not require scrolling to the request's history row"
    );
    wheel(visual, "project-native-actions", -3000.);
    click(visual, "answer-question-q-0");
    click(visual, "answer-question-q-1");
    assert_eq!(
        answers(&view, visual, "question")["q"],
        json!([" exact ", "β"])
    );
    click(visual, "answer-custom-question-q");
    visual.simulate_input("  custom answer  ");
    visual.run_until_parked();
    view.read_with(visual, |view, cx| {
        assert!(!view
            .native
            .actions
            .read(cx)
            .selections
            .contains_key(&("question".into(), "q".into())))
    });
    assert_eq!(
        answers(&view, visual, "question")["q"],
        json!("  custom answer  ")
    );
    click(visual, "answer-question-q-1");
    assert!(input(&view, visual, "question").read_with(visual, |input, _| input.value().is_empty()));
    click(visual, "answer-custom-question-q");
    visual.simulate_input("   ");
    visual.run_until_parked();
    assert_eq!(
        answers(&view, visual, "question")["q"],
        json!(["β"]),
        "whitespace alone does not erase choices"
    );
    visual.simulate_input("final custom");
    visual.run_until_parked();
    view.update(visual, |view, _| view.active = true);
    key(visual, "tab");
    key(visual, "space");
    view.read_with(visual, |view, cx| {
        let actions = view.native.actions.read(cx);
        assert_eq!(
            actions.records.len(),
            1,
            "real Tab/Space prepared the response"
        );
        let Intent::Respond {
            answers: Some(answers),
            ..
        } = &actions.records[0].command.intent
        else {
            panic!("question response");
        };
        assert_eq!(answers["q"], json!("   final custom"));
    });
    let after = bounds(visual, &format!("project-message-{position}"))
        .unwrap()
        .origin
        .y
        - visual.debug_bounds("project-history").unwrap().origin.y;
    assert_eq!(
        before, after,
        "typing, internal wheel and response preserve history's anchor"
    );
    assert_eq!(
        Some(logical_before),
        view.read_with(visual, |view, cx| view.feed.read(cx).test_scroll_anchor(cx)),
        "answering preserves the actual source-keyed list offset"
    );
}

#[gpui::test]
fn a_long_unconfirmed_message_cannot_bury_the_current_request(cx: &mut TestAppContext) {
    let (view, visual, _temp, _) = fixture(cx, 0);
    view.update(visual, |view, cx| {
        view.native.actions.update(cx, |actions, _| {
            actions.records.push(Record {
                command: okilum_t3::actions::Command::new(
                    THREAD,
                    Intent::message_with_attachments(
                        "Long retained message from uncertain delivery. ".repeat(300),
                        DispatchMode::StartImmediately,
                        Vec::new(),
                    ),
                ),
                sequence: None,
            });
        });
    });
    show(
        &view,
        visual,
        0,
        vec![request("question", "user_input_request")],
    );
    let panel = visual.debug_bounds("project-native-actions").unwrap();
    let form = visual.debug_bounds("project-current-request").unwrap();
    diagnostic_error!("short current form vs retained recovery: panel={panel:?}; form={form:?}");
    assert!(
        form.origin.y >= panel.origin.y && form.bottom() <= panel.bottom(),
        "the current form is visible before long recovery details"
    );
    for selector in [
        "respond-question",
        "question-attach-question-q",
        "question-paste-question-q",
    ] {
        let control = bounds(visual, selector)
            .expect("positive control: response and attachment actions remain available");
        assert!(
            control.origin.y >= panel.origin.y && control.bottom() <= panel.bottom(),
            "{selector} stays visible in the bounded short form"
        );
    }
    click(visual, "answer-question-q-0");
    assert_eq!(answers(&view, visual, "question")["q"], json!([" exact "]));
    click(visual, "answer-custom-question-q");
    visual.simulate_input("answer despite unconfirmed message");
    visual.run_until_parked();
    assert_eq!(
        answers(&view, visual, "question")["q"],
        json!("answer despite unconfirmed message")
    );
}

#[gpui::test]
fn forbidden_custom_is_not_rendered_or_sent_and_message_choices_keep_exact_values(
    cx: &mut TestAppContext,
) {
    let (view, visual, _temp, _) = fixture(cx, 0);
    let mut question = request("closed", "user_input_request");
    question.item["questions"][0]["allowCustomAnswer"] = json!(false);
    question.response_mode = Some("message".into());
    show(&view, visual, 0, vec![question]);
    assert!(visual.debug_bounds("project-request-closed").is_some());
    assert!(visual.debug_bounds("answer-custom-closed-q").is_none());
    click(visual, "answer-closed-q-0");
    click(visual, "answer-closed-q-1");
    input(&view, visual, "closed").update_in(visual, |input, window, cx| {
        input.set_value("must not leak", window, cx)
    });
    assert_eq!(
        answers(&view, visual, "closed")["q"],
        json!("[\" exact \",\"β\"]")
    );
    // #2321/#2324: activation restores the typed composer and can redraw
    // request controls. Exercise the settled, active surface like a real user.
    view.update(visual, |view, cx| {
        view.active = true;
        cx.notify();
    });
    visual.run_until_parked();
    click(visual, "respond-closed");
    view.read_with(visual, |view, cx| {
        let actions = view.native.actions.read(cx);
        assert_eq!(actions.records.len(), 1);
        let Intent::Respond {
            answers: Some(answers),
            ..
        } = &actions.records[0].command.intent
        else {
            panic!("question response");
        };
        assert_eq!(answers["q"], json!("[\" exact \",\"β\"]"));
    });
}

#[gpui::test]
fn unpinned_history_card_keeps_response_controls_working(cx: &mut TestAppContext) {
    struct Fallback {
        card: Entity<RequestCard>,
        _project: Entity<ProjectView>,
    }
    impl Render for Fallback {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(self.card.clone())
        }
    }
    cx.update(crate::test_init);
    let temp = tempfile::tempdir().unwrap();
    let mut project = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = super::super::super::fixture::view(
                temp.path().to_owned(),
                Arc::new(super::super::super::fixture::Fake::default()),
                window,
                cx,
            );
            view.active = false;
            super::tests::inert_native(&mut view);
            view.sync_native_action_surface(cx);
            assert!(view.native.client_can_operate());
            view.native.actions.update(cx, |actions, cx| {
                actions.set_pinned(false, cx);
                actions.set_enabled(true, cx);
                actions.set_state(
                    State {
                        ready: true,
                        requests: vec![request("fallback", "user_input_request")],
                        ..Default::default()
                    },
                    window,
                    cx,
                );
            });
            view
        });
        let actions = view.read(cx).native.actions.clone();
        let item = Arc::new(actions.read(cx).state.requests[0].item.clone());
        let card = cx.new(|cx| RequestCard::new(actions, item, true, cx));
        project = Some(view.clone());
        let fallback = cx.new(|_| Fallback {
            card,
            _project: view,
        });
        Root::new(fallback, window, cx)
    });
    visual.simulate_resize(size(px(760.), px(600.)));
    visual.run_until_parked();
    let project = project.unwrap();
    assert!(visual.debug_bounds("project-current-request").is_none());
    click(visual, "answer-fallback-q-0");
    assert_eq!(
        answers(&project, visual, "fallback")["q"],
        json!([" exact "])
    );
    click(visual, "answer-custom-fallback-q");
    visual.simulate_input("history answer");
    visual.run_until_parked();
    assert_eq!(
        answers(&project, visual, "fallback")["q"],
        json!("history answer")
    );
}

#[gpui::test]
fn reconnect_retains_one_input_but_scope_and_native_clear_retire_it(cx: &mut TestAppContext) {
    let (view, visual, _temp, _) = fixture(cx, 0);
    let question = request("retained", "user_input_request");
    show(&view, visual, 0, vec![question.clone()]);
    let original = input(&view, visual, "retained");
    click(visual, "answer-custom-retained-q");
    visual.simulate_input("same answer");
    visual.run_until_parked();
    view.update_in(visual, |view, window, cx| {
        view.native.actions.update(cx, |actions, cx| {
            actions.set_state(State::default(), window, cx)
        })
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("project-request-retained").is_some());
    assert_eq!(input(&view, visual, "retained"), original);
    click(visual, "respond-retained");
    view.read_with(visual, |view, cx| {
        assert!(
            view.native.actions.read(cx).records.is_empty(),
            "disconnected presentation grants no command authority"
        )
    });
    show(&view, visual, 0, vec![question.clone()]);
    assert_eq!(input(&view, visual, "retained"), original);
    assert_eq!(
        original.read_with(visual, |input, _| input.value().to_string()),
        "same answer"
    );
    view.update(visual, |view, cx| {
        let before = view.native.request_lease.clone();
        view.native.pause();
        assert!(
            Arc::ptr_eq(&before, &view.native.request_lease),
            "pause is the same conversation lease"
        );
        view.executors_open = true;
        view.sync_native_action_surface(cx);
        assert!(!view.native.actions.read(cx).pinned);
        cx.notify();
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("project-current-request").is_none());
    // Mounting the same feed without a pinned surface retains history controls.
    view.update(visual, |view, cx| {
        view.executors_open = false;
        view.native.client = None;
        view.sync_native_action_surface(cx);
        cx.notify();
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("project-request-retained").is_some());
    assert!(
        visual.debug_bounds("answer-custom-retained-q").is_some(),
        "history fallback remains interactive when no pinned panel is mounted"
    );
    view.update_in(visual, |view, window, cx| {
        super::tests::inert_native(view);
        view.chat = Some(Conversation::new("replacement"));
        view.binding.as_mut().unwrap().manager.thread_id = Some("replacement".into());
        view.sync_native_action_surface(cx);
        let mut next = request("new", "user_input_request");
        next.item["threadId"] = json!("replacement");
        view.native.actions.update(cx, |actions, cx| {
            assert!(actions.inputs.is_empty() && actions.display_requests.is_empty());
            actions.set_state(
                State {
                    ready: true,
                    requests: vec![next],
                    ..Default::default()
                },
                window,
                cx,
            );
        });
        cx.notify();
    });
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("project-request-new").is_some(),
        "new snapshot installed before first render survives scope synchronization"
    );
    view.update_in(visual, |view, window, cx| {
        view.environment = Some(Environment::new("other", "http://127.0.0.1:8").unwrap());
        view.sync_native_action_surface(cx);
        view.native.actions.update(cx, |actions, cx| {
            assert!(
                actions.inputs.is_empty() && actions.display_requests.is_empty(),
                "origin changes retire the previous draft snapshot"
            );
            let mut next = request("new", "user_input_request");
            next.item["threadId"] = json!("replacement");
            actions.set_state(
                State {
                    ready: true,
                    requests: vec![next],
                    ..Default::default()
                },
                window,
                cx,
            );
        });
        let before = view.native.request_lease.clone();
        view.native.clear();
        assert!(!Arc::ptr_eq(&before, &view.native.request_lease));
        view.sync_native_action_surface(cx);
        let actions = view.native.actions.read(cx);
        assert!(
            actions.inputs.is_empty()
                && actions.display_requests.is_empty()
                && actions.state.requests.is_empty()
        );
        cx.notify();
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("project-request-new").is_none());
}

#[gpui::test]
fn a_real_click_deferred_across_a_conversation_switch_cannot_answer_a_reused_request_id(
    cx: &mut TestAppContext,
) {
    let (view, visual, temp, armed) = fixture(cx, 0);
    show(&view, visual, 0, vec![request("same", "approval_request")]);
    // #2321/#2324: activation restores the typed composer and can redraw
    // request controls. Exercise the settled, active surface like a real user.
    view.update(visual, |view, cx| {
        view.active = true;
        cx.notify();
    });
    visual.run_until_parked();
    armed.set(true);
    click(visual, "approval-same-3");
    view.read_with(visual, |view, cx| {
        assert_eq!(view.chat.as_ref().unwrap().thread_id, "replacement");
        let actions = view.native.actions.read(cx);
        assert_eq!(
            actions.deferred_dispatches.get(),
            1,
            "positive control: the real click queued an action"
        );
        assert!(
            actions.state.allows(&Intent::Respond {
                attachments_by_question_id: Default::default(),
                request_id: "same".into(),
                decision: Some("accept".into()),
                answers: None
            }),
            "same request ID is actionable in the replacement scope"
        );
        assert!(
            actions.records.is_empty(),
            "old click must not publish under the new conversation"
        );
    });
    assert!(!temp.path().join("t3-native-commands").exists());
    click(visual, "approval-same-3");
    view.read_with(visual, |view, cx| {
        let actions = view.native.actions.read(cx);
        assert_eq!(
            actions.records.len(),
            1,
            "positive control: a fresh click still dispatches"
        );
        assert_eq!(actions.records[0].command.thread_id, "replacement");
    });
}

#[gpui::test]
async fn approval_fallback_buttons_send_exact_decisions_and_only_server_resolution_advances(
    cx: &mut TestAppContext,
) {
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        sync::{mpsc, Mutex},
        thread,
    };
    cx.executor().allow_parking();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let environment = Environment::new(
        "fixture",
        &format!("http://{}", listener.local_addr().unwrap()),
    )
    .unwrap();
    let received = Arc::new(Mutex::new(Vec::new()));
    let observed = received.clone();
    let (ready, actions) = mpsc::channel();
    let server = thread::spawn(move || {
        let accept = || {
            let start = std::time::Instant::now();
            loop {
                if let Ok((stream, _)) = listener.accept() {
                    // BSD/macOS inherits the listener's nonblocking mode.
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(4)))
                        .unwrap();
                    return stream;
                }
                assert!(
                    start.elapsed() < Duration::from_secs(8),
                    "positive control: approval must reach native RPC fixture"
                );
                thread::sleep(Duration::from_millis(5));
            }
        };
        for sequence in 1..=4 {
            // Rendering the next request is not part of its RPC deadline.
            // Arm the autonomous socket fixture only when its UI is ready.
            assert_eq!(actions.recv().unwrap(), sequence);
            let mut stream = accept();
            let mut reader = BufReader::new(&mut stream);
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
            }
            drop(reader);
            let body = json!({"ticket":"fixture"}).to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            drop(stream);
            let mut ws = tungstenite::accept(accept()).unwrap();
            let wire: Value = serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(wire["tag"], "orchestration.dispatchCommand");
            observed.lock().unwrap().push(wire["payload"].clone());
            ws.send(tungstenite::Message::Text(json!({"_tag":"Exit","requestId":"1","exit":{"_tag":"Success","value":{"sequence":sequence}}}).to_string().into())).unwrap();
        }
    });
    let (view, visual, _temp, _) = fixture(cx, 0);
    view.update(visual, |view, cx| {
        let token = NativeToken::from_secret(&environment, &serde_json::to_vec(&json!({
            "origin":environment.origin(),"access_token":"fixture","expires_at":chrono::Utc::now().timestamp() as u64+3600,
            "scope":"orchestration:read orchestration:operate"
        })).unwrap()).unwrap();
        view.environment = Some(environment.clone());
        view.native.client = Some(Arc::new(NativeClient::new(environment, token, Duration::from_secs(3)).unwrap()));
        view.active = true;
        view.sync_native_action_surface(cx);
    });
    for (index, (decision, _)) in APPROVAL_FALLBACK_OPTIONS.iter().enumerate() {
        let id = format!("approval-{index}");
        let approval = request(&id, "approval_request");
        let next = request("approval-1", "approval_request");
        let pending = if index == 0 {
            vec![request("question", "user_input_request"), approval, next]
        } else {
            vec![approval]
        };
        show(&view, visual, 0, pending);
        assert!(visual.debug_bounds("project-current-request").is_some());
        for option in 0..4 {
            assert!(bounds(visual, &format!("approval-{id}-{option}")).is_some());
        }
        if index == 0 {
            assert!(visual
                .debug_bounds("project-current-request-count-2")
                .is_some());
            assert!(
                visual.debug_bounds("respond-question").is_none(),
                "T3 approval priority precedes questions"
            );
        }
        ready.send((index + 1) as u64).unwrap();
        click(visual, &format!("approval-{id}-{index}"));
        let actions = view.read_with(visual, |view, _| view.native.actions.clone());
        // Parking a test executor is not an acknowledgement from the real
        // socket worker. Observe publication before inspecting its receipt.
        visual
            .condition(&actions, |actions, _| {
                actions
                    .records
                    .get(index)
                    .is_some_and(|record| record.sequence.is_some())
                    || actions.error.is_some()
            })
            .await;
        view.read_with(visual, |view, cx| {
            let actions = view.native.actions.read(cx);
            assert_eq!(actions.records.len(), index + 1);
            let expected = Intent::Respond {
                attachments_by_question_id: Default::default(),
                request_id: id.clone(),
                decision: Some((*decision).into()),
                answers: None,
            };
            let record = actions
                .records
                .iter()
                .find(|record| record.command.intent == expected)
                .expect("the receipt retains the exact request identity and full intent");
            assert_eq!(
                record.sequence,
                Some((index + 1) as u64),
                "native RPC acknowledged this exact action with its sequence"
            );
            assert_eq!(
                actions.current_request().unwrap().0.id,
                id,
                "ACK alone does not advance the request"
            );
            assert_eq!(record.command.intent, expected);
        });
        if index == 0 {
            assert!(visual.debug_bounds("project-request-approval-0").is_some());
            assert!(visual.debug_bounds("project-request-approval-1").is_none());
            show(
                &view,
                visual,
                0,
                vec![request("approval-1", "approval_request")],
            );
            assert!(
                visual.debug_bounds("project-request-approval-1").is_some(),
                "server resolution reveals the next approval"
            );
            assert!(visual
                .debug_bounds("project-current-request-count-2")
                .is_none());
        }
    }
    server.join().unwrap();
    let wire = received.lock().unwrap();
    assert_eq!(wire.len(), 4);
    for (index, (decision, _)) in APPROVAL_FALLBACK_OPTIONS.iter().enumerate() {
        assert_eq!(wire[index]["decision"], *decision);
        assert_eq!(wire[index]["requestId"], format!("approval-{index}"));
        assert_eq!(wire[index]["threadId"], THREAD);
        if index > 0 {
            assert_ne!(wire[index]["commandId"], wire[index - 1]["commandId"]);
        }
    }
    drop(wire);
    let mut explicit = request("explicit", "approval_request");
    explicit.item["options"] = json!([{"decision":"acceptAlways","label":"Provider option","warning":"Provider warning stays visible"}]);
    show(&view, visual, 0, vec![explicit]);
    assert!(visual.debug_bounds("approval-explicit-0").is_some());
    assert!(
        visual.debug_bounds("approval-explicit-1").is_none(),
        "provider options are not supplemented with fallback decisions"
    );
    for value in [json!(null), json!([]), json!({"invalid":true})] {
        let mut explicit = request("empty", "approval_request");
        explicit.item["options"] = value;
        show(&view, visual, 0, vec![explicit]);
        assert!(visual.debug_bounds("project-request-empty").is_some());
        assert!(
            visual.debug_bounds("approval-empty-0").is_none(),
            "present invalid/empty options never trigger fallback"
        );
    }
}
