use super::*;
use ::core::prelude::v1::test;
use std::{
    cell::{Cell, RefCell},
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    rc::Rc,
    sync::Mutex,
    thread,
};

const THREAD: &str = "mcp:fixture-manager";
const ID: &str = "wizard";

fn choice(id: &str, value: &str, custom: bool) -> Value {
    json!({"id":id,"header":format!("Step {id}"),"question":format!("Answer {id}"),"allowCustomAnswer":custom,"options":[{"label":format!("Choose {id}"),"value":value}]})
}
fn request(questions: Vec<Value>) -> Request {
    Request {
        id: ID.into(),
        item: json!({"id":"wizard-row","threadId":THREAD,"requestId":ID,"type":"user_input_request","status":"pending","questions":questions}),
        answerable: true,
        dismissible: true,
        response_mode: Some("message".into()),
        reason: None,
    }
}
fn three() -> Request {
    request(vec![
        choice("one", " exact ", true),
        json!({"id":"two","header":"Explain","question":"Why?","options":[]}),
        choice("three", "β final", false),
    ])
}
struct Host {
    view: Entity<ProjectView>,
    switch: Rc<Cell<bool>>,
    switches: Rc<Cell<usize>>,
    replacement: Rc<RefCell<Option<Request>>>,
}
impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let switch = self.switch.clone();
        let switches = self.switches.clone();
        let replacement = self.replacement.clone();
        let view = self.view.clone();
        div()
            .size_full()
            .capture_any_mouse_up(move |_, window, cx| {
                if !switch.replace(false) {
                    return;
                }
                switches.set(switches.get() + 1);
                view.update(cx, |view, cx| {
                    view.chat = Some(Conversation::new("replacement"));
                    view.binding.as_mut().unwrap().manager.thread_id = Some("replacement".into());
                    view.sync_native_action_surface(cx);
                    let mut next = replacement.borrow().as_ref().unwrap().clone();
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
                        )
                    });
                    cx.notify();
                });
            })
            .child(self.view.clone())
    }
}
type Fixture<'a> = (
    Entity<ProjectView>,
    &'a mut VisualTestContext,
    tempfile::TempDir,
    Rc<Cell<bool>>,
    Rc<Cell<usize>>,
    Rc<RefCell<Option<Request>>>,
);

pub(super) fn fixture(cx: &mut TestAppContext, environment: Option<Environment>) -> Fixture<'_> {
    // The autonomous RPC fixture uses real sockets; it never waits for a UI step.
    cx.executor().allow_parking();
    cx.update(|cx| {
        crate::test_init(cx);
        cx.set_reduce_motion(true);
    });
    let temp = tempfile::tempdir().unwrap();
    let armed = Rc::new(Cell::new(false));
    let switches = Rc::new(Cell::new(0));
    let replacement = Rc::new(RefCell::new(None));
    let mut project = None;
    let (_,visual)=cx.add_window_view(|window,cx| {
        let view=cx.new(|cx| {
            let mut view=super::super::super::fixture::view(temp.path().to_owned(),Arc::new(super::super::super::fixture::Fake::default()),window,cx);
            view.active=false; super::tests::inert_native(&mut view);
            {
                let environment=environment.unwrap_or_else(|| Environment::new("wizard-no-rpc","http://127.0.0.1:9").unwrap());
                let token=NativeToken::from_secret(&environment,&serde_json::to_vec(&json!({"origin":environment.origin(),"access_token":"wizard-fixture","expires_at":chrono::Utc::now().timestamp() as u64+3600,"scope":"orchestration:read orchestration:operate"})).unwrap()).unwrap();
                view.native.client=Some(Arc::new(NativeClient::new(environment.clone(),token,Duration::from_secs(3)).unwrap())); view.environment=Some(environment);
            }
            view.sync_native_action_surface(cx); view
        });
        project=Some(view.clone());
        let host=cx.new(|_| Host {view,switch:armed.clone(),switches:switches.clone(),replacement:replacement.clone()});
        Root::new(host,window,cx)
    });
    visual.simulate_resize(size(px(760.), px(1000.)));
    visual.run_until_parked();
    let view = project.unwrap();
    view.read_with(visual, |view, _| {
        assert!(
            view.native.client_can_operate() && view.native.stream_error_is_clear(),
            "positive control: every wizard fixture has a ready scoped client"
        )
    });
    view.update(visual, |view, _| view.active = true);
    (view, visual, temp, armed, switches, replacement)
}
pub(super) fn show(view: &Entity<ProjectView>, visual: &mut VisualTestContext, request: Request) {
    view.update_in(visual, |view, window, cx| {
        view.native.stream_ready = true;
        view.sync_native_action_surface(cx);
        view.native.actions.update(cx, |actions, cx| {
            actions.set_state(
                State {
                    ready: true,
                    requests: vec![request],
                    ..Default::default()
                },
                window,
                cx,
            );
            actions.set_enabled(true, cx);
        });
        cx.notify();
    });
    visual.run_until_parked();
}
pub(super) fn click(visual: &mut VisualTestContext, selector: &'static str) {
    let bounds = visual
        .debug_bounds(selector)
        .expect("positive control: real wizard button rendered");
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
pub(super) fn tick(visual: &mut VisualTestContext, millis: u64) {
    visual
        .executor()
        .advance_clock(Duration::from_millis(millis));
    visual.run_until_parked();
}
fn active(view: &Entity<ProjectView>, visual: &VisualTestContext) -> String {
    view.read_with(visual, |view, cx| {
        view.native.actions.read(cx).question_progress[ID]
            .active()
            .unwrap()
            .into()
    })
}
pub(super) fn input(
    view: &Entity<ProjectView>,
    visual: &VisualTestContext,
    id: &str,
) -> Entity<InputState> {
    view.read_with(visual, |view, cx| {
        view.native.actions.read(cx).inputs[&(ID.into(), id.into())].clone()
    })
}
pub(super) fn rpc(ack: bool) -> (Environment, Arc<Mutex<Vec<Value>>>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let environment = Environment::new(
        "wizard",
        &format!("http://{}", listener.local_addr().unwrap()),
    )
    .unwrap();
    let received = Arc::new(Mutex::new(Vec::new()));
    let observed = received.clone();
    let server = thread::spawn(move || {
        let accept = || {
            let start = std::time::Instant::now();
            loop {
                if let Ok((stream, _)) = listener.accept() {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(4)))
                        .unwrap();
                    return stream;
                }
                assert!(
                    start.elapsed() < Duration::from_secs(8),
                    "positive control: final response reached the actual RPC fixture"
                );
                thread::sleep(Duration::from_millis(5));
            }
        };
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
        let body = json!({"ticket":"wizard-ticket"}).to_string();
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
        if ack {
            ws.send(tungstenite::Message::Text(json!({"_tag":"Exit","requestId":"1","exit":{"_tag":"Success","value":{"sequence":1}}}).to_string().into())).unwrap();
        }
    });
    (environment, received, server)
}

#[gpui::test]
fn header_collapse_while_submitting_preserves_draft_and_message_dismiss_sends_only_dismiss(
    cx: &mut TestAppContext,
) {
    let (environment, wire, server) = rpc(true);
    let (view, visual, _temp, _, _, _) = fixture(cx, Some(environment));
    let message = request(vec![json!({
        "id":"one", "header":"Explain", "question":"Keep this draft?", "options":[]
    })]);
    let mut live = message.clone();
    live.response_mode = Some("live".into());
    live.dismissible = false;
    show(&view, visual, live);
    assert!(
        visual.debug_bounds("dismiss-wizard").is_none(),
        "live capability exposes no Dismiss control"
    );
    view.read_with(visual, |view, cx| {
        assert!(
            !view.native.actions.read(cx).state.allows(&Intent::Dismiss {
                request_id: ID.into()
            })
        );
    });
    click(visual, "answer-custom-wizard-one");
    visual.simulate_input("  unsent exact draft  ");
    visual.run_until_parked();
    let draft = input(&view, visual, "one");
    show(&view, visual, message);
    assert_eq!(input(&view, visual, "one"), draft);
    view.read_with(visual, |view, cx| {
        assert!(
            view.native.actions.read(cx).state.allows(&Intent::Dismiss {
                request_id: ID.into()
            }),
            "positive control: message capability permits dismissal"
        );
    });
    view.update(visual, |view, cx| {
        view.busy = true;
        view.native.actions.update(cx, |actions, cx| {
            actions.busy = true;
            actions
                .question_progress
                .get_mut(ID)
                .unwrap()
                .begin_submission();
            cx.notify();
        });
        cx.notify();
    });
    visual.run_until_parked();
    click(visual, "question-header-wizard-one");
    assert!(
        visual.debug_bounds("question-collapsed-wizard").is_some(),
        "header remains a presentation-only disclosure during submission, matching T3"
    );
    click(visual, "dismiss-wizard");
    view.read_with(visual, |view, cx| {
        let actions = view.native.actions.read(cx);
        assert!(actions.busy && actions.question_progress[ID].submitting);
        assert!(actions.question_progress[ID].collapsed());
        assert!(actions.records.is_empty());
        assert_eq!(
            actions.deferred_dispatches.get(),
            0,
            "the independently disabled Dismiss never dispatches while submitting"
        );
    });
    assert!(wire.lock().unwrap().is_empty());
    assert_eq!(
        draft.read_with(visual, |input, _| input.value().to_string()),
        "  unsent exact draft  "
    );

    view.update(visual, |view, cx| {
        view.busy = false;
        view.native.actions.update(cx, |actions, cx| {
            actions.busy = false;
            actions
                .question_progress
                .get_mut(ID)
                .unwrap()
                .finish_submission();
            cx.notify();
        });
        cx.notify();
    });
    visual.run_until_parked();
    click(visual, "dismiss-wizard");
    server.join().unwrap();
    let commands = wire.lock().unwrap();
    assert_eq!(
        commands.len(),
        1,
        "positive RPC detector: only the ready message Dismiss reaches transport"
    );
    assert_eq!(commands[0]["type"], "thread.user-input.dismiss");
    assert_eq!(commands[0]["threadId"], THREAD);
    assert_eq!(commands[0]["requestId"], ID);
    assert!(commands[0].get("answers").is_none());
    assert!(commands[0].get("decision").is_none());
    view.read_with(visual, |view, cx| {
        let actions = view.native.actions.read(cx);
        assert_eq!(actions.deferred_dispatches.get(), 1);
        assert_eq!(actions.records.len(), 1);
        assert_eq!(actions.records[0].sequence, Some(1));
        assert_eq!(
            actions.records[0].command.command_id,
            commands[0]["commandId"].as_str().unwrap()
        );
        assert!(matches!(&actions.records[0].command.intent,
            Intent::Dismiss { request_id } if request_id == ID));
        assert!(
            actions.question_progress[ID].collapsed(),
            "X Dismiss does not toggle disclosure; ACK alone is not canonical resolution"
        );
    });
    assert_eq!(input(&view, visual, "one"), draft);
    assert_eq!(
        draft.read_with(visual, |input, _| input.value().to_string()),
        "  unsent exact draft  "
    );
    assert!(visual.debug_bounds("question-collapsed-wizard").is_some());
}

#[gpui::test]
fn three_steps_preserve_actual_inputs_collapse_and_previous_then_send_all_answers_once(
    cx: &mut TestAppContext,
) {
    let (environment, wire, server) = rpc(true);
    let (view, visual, _temp, _, _, _) = fixture(cx, Some(environment));
    show(&view, visual, three());
    assert!(visual
        .debug_bounds("question-progress-wizard-1-3")
        .is_some());
    click(visual, "question-next-wizard");
    assert_eq!(
        active(&view, visual),
        "one",
        "required incomplete answer cannot advance"
    );
    let submitting = Rc::new(Cell::new(false));
    let actions = view.read_with(visual, |view, _| view.native.actions.clone());
    let seen = submitting.clone();
    let _observer: Entity<()> = visual.update(|_, cx| {
        cx.new(|cx| {
            cx.observe(&actions, move |_, actions, cx| {
                if actions
                    .read(cx)
                    .question_progress
                    .get(ID)
                    .is_some_and(|progress| progress.submitting)
                {
                    seen.set(true);
                }
            })
            .detach();
        })
    });
    click(visual, "answer-wizard-one-0");
    view.read_with(visual, |view, cx| {
        assert_eq!(
            view.native.actions.read(cx).selections[&(ID.into(), "one".into())],
            vec![" exact "]
        )
    });
    tick(visual, 199);
    assert_eq!(active(&view, visual), "one");
    tick(visual, 1);
    assert_eq!(active(&view, visual), "two");
    assert!(visual
        .debug_bounds("question-progress-wizard-2-3")
        .is_some());
    let draft = input(&view, visual, "two");
    click(visual, "answer-custom-wizard-two");
    visual.simulate_input("  exact explanation  ");
    visual.run_until_parked();
    key(visual, "shift-tab");
    key(visual, "shift-tab");
    key(visual, "space");
    assert!(
        visual.debug_bounds("question-collapsed-wizard").is_some(),
        "physical Space collapses the actual question header"
    );
    assert!(visual.debug_bounds("answer-custom-wizard-two").is_none());
    assert_eq!(
        draft.read_with(visual, |input, _| input.value().to_string()),
        "  exact explanation  "
    );
    key(visual, "space");
    assert!(visual.debug_bounds("answer-custom-wizard-two").is_some());
    click(visual, "question-previous-wizard");
    assert_eq!(active(&view, visual), "one");
    tick(visual, 500);
    assert_eq!(
        active(&view, visual),
        "one",
        "rendering an already selected answer never rearms auto advance"
    );
    click(visual, "question-next-wizard");
    assert_eq!(active(&view, visual), "two");
    assert_eq!(input(&view, visual, "two"), draft);
    click(visual, "question-next-wizard");
    assert_eq!(active(&view, visual), "three");
    click(visual, "answer-wizard-three-0");
    tick(visual, 199);
    assert!(wire.lock().unwrap().is_empty());
    tick(visual, 1);
    server.join().unwrap();
    assert!(
        submitting.get(),
        "positive control: request-scoped Submitting was observable before deferred completion"
    );
    let commands = wire.lock().unwrap();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0]["type"], "runtime-request.respond");
    assert_eq!(commands[0]["requestId"], ID);
    assert_eq!(
        commands[0]["answers"],
        json!({"one":" exact ","two":"  exact explanation  ","three":"β final"})
    );
    view.read_with(visual, |view, cx| {
        let actions = view.native.actions.read(cx);
        assert_eq!(actions.records.len(), 1);
        assert_eq!(actions.records[0].sequence, Some(1));
        assert_eq!(actions.deferred_dispatches.get(), 1);
        assert!(!actions.question_progress[ID].submitting);
        assert_eq!(
            actions.records[0].command.command_id,
            commands[0]["commandId"].as_str().unwrap()
        );
    });
    drop(commands);
    click(visual, "respond-wizard");
    tick(visual, 500);
    assert_eq!(wire.lock().unwrap().len(), 1);
    assert_eq!(
        active(&view, visual),
        "three",
        "ACK alone does not resolve or advance the request"
    );
    view.read_with(visual, |view, cx| {
        assert_eq!(
            view.native.actions.read(cx).deferred_dispatches.get(),
            1,
            "acknowledged retention disables fresh submission"
        )
    });
}

#[gpui::test]
fn repeat_single_click_restarts_exact_200ms_and_multi_and_optional_never_auto_advance(
    cx: &mut TestAppContext,
) {
    let (view, visual, _temp, _, _, _) = fixture(cx, None);
    let mut multi = choice("two", " β ", false);
    multi["multiSelect"] = json!(true);
    multi["options"] = json!([{"value":" β ","label":"Beta"},{"value":"exact","label":"Exact"}]);
    let mut first = choice("one", " exact ", true);
    first["initialAnswer"] = json!(" exact ");
    show(
        &view,
        visual,
        request(vec![
            first,
            multi,
            json!({"id":"three","question":"Optional","required":false,"options":[]}),
        ]),
    );
    tick(visual, 500);
    assert_eq!(
        active(&view, visual),
        "one",
        "initialAnswer seeds completeness without scheduling a timer"
    );
    click(visual, "answer-wizard-one-0");
    tick(visual, 199);
    click(visual, "answer-wizard-one-0");
    tick(visual, 1);
    assert_eq!(active(&view, visual), "one");
    view.read_with(visual, |view, cx| {
        assert_eq!(
            view.native.actions.read(cx).selections[&(ID.into(), "one".into())],
            vec![" exact "]
        )
    });
    tick(visual, 198);
    assert_eq!(active(&view, visual), "one");
    tick(visual, 1);
    assert_eq!(active(&view, visual), "two");
    click(visual, "answer-wizard-two-0");
    click(visual, "answer-wizard-two-1");
    tick(visual, 1000);
    assert_eq!(
        active(&view, visual),
        "two",
        "multi selection never schedules automatic progression"
    );
    view.read_with(visual, |view, cx| {
        let actions = view.native.actions.read(cx);
        let Intent::Respond {
            answers: Some(answers),
            ..
        } = actions.intent(ID, cx)
        else {
            panic!("question intent")
        };
        assert_eq!(answers["two"], json!("[\" β \",\"exact\"]"));
    });
    click(visual, "question-next-wizard");
    assert_eq!(active(&view, visual), "three");
    let optional = request(vec![
        json!({"id":"one","question":"Optional first","required":false,"options":[]}),
        json!({"id":"two","question":"Optional last","required":false,"options":[]}),
    ]);
    show(
        &view,
        visual,
        Request {
            id: "optional".into(),
            item: json!({"id":"optional-row","type":"user_input_request","requestId":"optional","threadId":THREAD,"questions":optional.item["questions"]}),
            ..optional
        },
    );
    click(visual, "question-next-optional");
    view.read_with(visual, |view, cx| {
        let actions = view.native.actions.read(cx);
        assert_eq!(actions.question_progress["optional"].active(), Some("two"));
        assert!(
            !actions.state.allows(&actions.intent("optional", cx)),
            "all optional omissions do not invent a valid empty final payload"
        );
    });
    click(visual, "respond-optional");
    view.read_with(visual, |view, cx| {
        assert!(view.native.actions.read(cx).records.is_empty())
    });
}

#[gpui::test]
fn pending_timers_are_invalidated_without_rearming_after_edits_navigation_and_authority_changes(
    cx: &mut TestAppContext,
) {
    let (view, visual, _temp, _, _, _) = fixture(cx, None);
    let template = request(vec![
        choice("one", " exact ", true),
        choice("two", "second", true),
        json!({"id":"three","question":"Last required","options":[]}),
    ]);
    for case in [
        "custom",
        "collapse",
        "previous",
        "disabled",
        "reconnect",
        "scope",
        "replacement",
        "priority",
        "retained",
        "resolved",
        "parent-paused",
    ] {
        view.update_in(visual, |view, window, cx| {
            view.active = true;
            view.native.stream_ready = true;
            view.native.actions.update(cx, |actions, cx| {
                actions.records.clear();
                actions.error = None;
                actions.set_state(
                    State {
                        ready: true,
                        ..Default::default()
                    },
                    window,
                    cx,
                );
            });
        });
        show(&view, visual, template.clone());
        click(visual, "answer-wizard-one-0");
        if case == "previous" {
            tick(visual, 200);
            assert_eq!(active(&view, visual), "two");
            click(visual, "answer-wizard-two-0");
        }
        view.read_with(visual, |view, cx| {
            assert!(
                view.native
                    .actions
                    .read(cx)
                    .question_timers
                    .contains_key(ID),
                "positive control: {case} began with an armed timer"
            )
        });
        match case {
            "custom" => {click(visual,"answer-custom-wizard-one"); visual.simulate_input("custom overrides the choice"); visual.run_until_parked();}
            "collapse" => click(visual,"question-header-wizard-one"),
            "previous" => click(visual,"question-previous-wizard"),
            "disabled" => view.update(visual,|view,cx|view.native.actions.update(cx,|actions,cx|{actions.set_enabled(false,cx); actions.set_enabled(true,cx);})),
            "reconnect" => view.update_in(visual,|view,window,cx|view.native.actions.update(cx,|actions,cx|{let ready=actions.state.clone(); let mut reconnecting=ready.clone(); reconnecting.ready=false; actions.set_state(reconnecting,window,cx); actions.set_state(ready,window,cx);})),
            "scope" => {view.update(visual,|view,cx|{view.native.clear(); super::tests::inert_native(view); view.sync_native_action_surface(cx);}); show(&view,visual,template.clone());}
            "replacement" => {let mut replacement=template.clone(); replacement.item["id"]=json!("replacement-row"); show(&view,visual,replacement);}
            "priority" => view.update_in(visual,|view,window,cx|view.native.actions.update(cx,|actions,cx| {
                let ready=actions.state.clone(); let mut queued=ready.clone(); queued.requests.push(Request{id:"approval".into(),item:json!({"id":"approval-row","type":"approval_request","requestId":"approval","threadId":THREAD}),answerable:true,dismissible:false,response_mode:Some("live".into()),reason:None}); actions.set_state(queued,window,cx); actions.set_state(ready,window,cx);
            })),
            "retained" => view.update(visual,|view,cx|view.native.actions.update(cx,|actions,cx| {
                actions.records.push(Record {command:okilum_t3::actions::Command::new(THREAD,Intent::Respond { attachments_by_question_id: Default::default(),request_id:ID.into(),decision:None,answers:Some(BTreeMap::from([("one".into(),json!(" exact ")),("two".into(),json!("second")),("three".into(),json!("complete"))]))}),sequence:None}); actions.cancel_retained_transitions(); cx.notify();
            })),
            "resolved" => view.update_in(visual,|view,window,cx|view.native.actions.update(cx,|actions,cx|actions.set_state(State {ready:true,..Default::default()},window,cx))),
            "parent-paused" => view.update(visual,|view,_|view.active=false),
            _=>unreachable!(),
        }
        tick(visual, 200);
        view.read_with(visual, |view, cx| {
            let actions = view.native.actions.read(cx);
            if case == "resolved" {
                assert!(!actions.question_progress.contains_key(ID));
            } else {
                assert_eq!(
                    actions.question_progress[ID].active(),
                    Some("one"),
                    "{case}: stale callback did not move a question"
                );
            }
            assert_eq!(
                actions.records.len(),
                usize::from(case == "retained"),
                "{case}: no fresh response was retained"
            );
        });
        if case == "parent-paused" {
            view.update(visual, |view, _| view.active = true);
            tick(visual, 500);
            assert_eq!(active(&view, visual), "one");
        }
    }
    view.update_in(visual, |view, window, cx| {
        view.native.actions.update(cx, |actions, cx| {
            actions.records.clear();
            actions.set_state(
                State {
                    ready: true,
                    ..Default::default()
                },
                window,
                cx,
            );
        })
    });
    show(&view, visual, template);
    click(visual, "answer-wizard-one-0");
    tick(visual, 200);
    assert_eq!(
        active(&view, visual),
        "two",
        "positive control: a fresh valid timer still advances"
    );
}

#[gpui::test]
fn a_real_old_option_mouseup_cannot_rebind_to_a_replacement_scope_with_reused_ids(
    cx: &mut TestAppContext,
) {
    let (view, visual, _temp, armed, switches, replacement) = fixture(cx, None);
    let next = request(vec![
        choice("one", " exact ", true),
        json!({"id":"two","question":"Next","options":[]}),
    ]);
    show(&view, visual, next.clone());
    *replacement.borrow_mut() = Some(next);
    let old = view.read_with(visual, |view, cx| {
        let actions = view.native.actions.read(cx);
        QuestionControl {
            conversation: actions.conversation.clone(),
            lease: actions.lease.clone(),
            transition: actions.question_progress[ID].transition(ID).unwrap(),
        }
    });
    armed.set(true);
    click(visual, "answer-wizard-one-0");
    assert_eq!(
        switches.get(),
        1,
        "positive control: scope changed in actual mouse-up capture before the old callback"
    );
    view.read_with(visual, |view, cx| {
        assert_eq!(view.chat.as_ref().unwrap().thread_id, "replacement");
        let actions = view.native.actions.read(cx);
        assert!(!actions.question_control_current(&old));
        assert!(actions.selections.is_empty());
        assert!(actions.question_timers.is_empty());
        assert!(actions.records.is_empty());
        let fresh = QuestionControl {
            conversation: actions.conversation.clone(),
            lease: actions.lease.clone(),
            transition: actions.question_progress[ID].transition(ID).unwrap(),
        };
        assert!(
            actions.question_control_current(&fresh),
            "positive control: the fresh control has valid provenance"
        );
    });
    tick(visual, 500);
    assert_eq!(active(&view, visual), "one");
    view.update(visual, |_, cx| cx.notify());
    visual.run_until_parked();
    click(visual, "answer-wizard-one-0");
    tick(visual, 200);
    assert_eq!(
        active(&view, visual),
        "two",
        "a fresh real click is not blocked by the old-control guard"
    );
}

#[gpui::test]
fn pre_retain_failure_releases_submitting_but_unknown_ack_keeps_the_same_retained_response(
    cx: &mut TestAppContext,
) {
    let (environment, wire, server) = rpc(false);
    let (view, visual, temp, _, _, _) = fixture(cx, Some(environment));
    show(
        &view,
        visual,
        request(vec![json!({"id":"one","question":"Explain","options":[]})]),
    );
    click(visual, "answer-custom-wizard-one");
    visual.simulate_input("retained exact text");
    visual.run_until_parked();
    let obstruction = temp.path().join("t3-native-commands");
    std::fs::write(&obstruction, b"not a directory").unwrap();
    click(visual, "respond-wizard");
    view.read_with(visual, |view, cx| {
        let actions = view.native.actions.read(cx);
        assert!(!view.busy && !actions.busy);
        assert!(
            !actions.question_progress[ID].submitting,
            "prepare failure before retain releases the request latch"
        );
        assert!(actions.records.is_empty());
        assert!(actions.error.is_some());
    });
    assert!(
        wire.lock().unwrap().is_empty(),
        "journal preparation failed before native dispatch"
    );
    std::fs::remove_file(obstruction).unwrap();
    click(visual, "respond-wizard");
    server.join().unwrap();
    let first = view.read_with(visual, |view, cx| {
        let actions = view.native.actions.read(cx);
        assert_eq!(actions.records.len(), 1);
        assert_eq!(actions.records[0].sequence, None);
        assert!(!actions.question_progress[ID].submitting);
        assert!(actions.request_retained(ID));
        assert!(actions.error.is_some());
        actions.records[0].command.clone()
    });
    assert_eq!(
        wire.lock().unwrap()[0]["answers"],
        json!({"one":"retained exact text"})
    );
    click(visual, "respond-wizard");
    tick(visual, 500);
    view.read_with(visual, |view, cx| {
        let actions = view.native.actions.read(cx);
        assert_eq!(actions.records.len(), 1);
        assert_eq!(actions.records[0].command.command_id, first.command_id);
        assert_eq!(
            actions.records[0].command.wire().unwrap(),
            first.wire().unwrap()
        );
    });
    assert_eq!(
        wire.lock().unwrap().len(),
        1,
        "unknown acceptance never permits a fresh second answer"
    );
    view.read_with(visual, |view, cx| {
        assert_eq!(
            view.native.actions.read(cx).deferred_dispatches.get(),
            2,
            "retained response disables a fresh dispatch attempt, not just a second receipt"
        )
    });
}
