use super::*;
use ::core::prelude::v1::test;
use gpui_component::WindowExt as _;

const ID: &str = "options";
const THREAD: &str = "mcp:fixture-manager";

type Fixture<'a> = (
    Entity<ProjectView>,
    &'a mut VisualTestContext,
    tempfile::TempDir,
);

fn fixture(cx: &mut TestAppContext) -> Fixture<'_> {
    cx.executor().allow_parking();
    cx.update(|cx| {
        crate::test_init(cx);
        cx.set_reduce_motion(true);
    });
    let temp = tempfile::tempdir().unwrap();
    let (root, visual) = cx.add_window_view(|window, cx| {
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
            view
        });
        Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(760.), px(1000.)));
    visual.run_until_parked();
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<ProjectView>().unwrap()
    });
    view.update(visual, |view, _| view.active = true);
    (view, visual, temp)
}

fn request(multi: bool) -> Request {
    Request {
        approval_detail: None,
        id: ID.into(),
        item: json!({"id":"options-row","threadId":THREAD,"requestId":ID,
        "type":"user_input_request","status":"pending","questions":[
            {"id":"first","header":"Choose","question":"Choose the exact value","multiSelect":multi,
             "options":[{"label":"First choice","value":" exact first "},
                        {"label":"Second choice","value":"β second"}]},
            {"id":"last","header":"Several","question":"Choose all that apply","multiSelect":true,
             "options":[{"label":"Next first","value":" final one "},
                        {"label":"Next second","value":"final β"}]}
        ]}),
        answerable: true,
        dismissible: true,
        response_mode: Some("message".into()),
        reason: None,
    }
}

fn show(view: &Entity<ProjectView>, visual: &mut VisualTestContext, request: Request) {
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

fn click(visual: &mut VisualTestContext, selector: &'static str) {
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("positive control: {selector} is rendered"));
    visual.simulate_click(bounds.center(), Modifiers::default());
    visual.run_until_parked();
}

fn key(visual: &mut VisualTestContext, name: &str, held: bool) {
    let keystroke = gpui::Keystroke::parse(name).unwrap();
    send_key(visual, keystroke, held);
}

fn focus_composer_control(view: &Entity<ProjectView>, visual: &mut VisualTestContext) {
    view.update_in(visual, |view, window, cx| {
        view.compose.update(cx, |input, cx| input.focus(window, cx));
    });
    // Kit buttons deliberately preserve mouse focus. Use real Tab navigation
    // to the following toolbar control before exercising bubbling keys.
    // The textarea is above its stable composer toolbar. Going backward now
    // reaches the question card, whose controls change during auto-advance.
    key(visual, "tab", false);
    visual.update(|window, cx| {
        assert!(
            window.focused(cx).is_some(),
            "positive control: Tab established keyboard focus"
        );
        assert!(
            !window.has_focused_input(cx),
            "positive control: the following toolbar control is noneditable"
        );
        assert!(
            !view
                .read(cx)
                .compose
                .read(cx)
                .focus_handle(cx)
                .is_focused(window),
            "positive control: focus left the raw composer handle"
        );
        assert!(
            !window
                .context_stack()
                .iter()
                .any(|context| context.contains("Input")),
            "positive control: the focused dispatch path contains no Input context"
        );
    });
}

fn logical_key(visual: &mut VisualTestContext, physical: &str, logical: &str) {
    let mut keystroke = gpui::Keystroke::parse(physical).unwrap();
    keystroke.key_char = Some(logical.into());
    send_key(visual, keystroke, false);
}

fn send_key(visual: &mut VisualTestContext, keystroke: gpui::Keystroke, held: bool) {
    visual.simulate_event(gpui::KeyDownEvent {
        keystroke: keystroke.clone(),
        is_held: held,
        prefer_character_input: false,
    });
    visual.simulate_event(gpui::KeyUpEvent { keystroke });
    visual.run_until_parked();
}

fn tick(visual: &mut VisualTestContext, millis: u64) {
    visual
        .executor()
        .advance_clock(Duration::from_millis(millis));
    visual.run_until_parked();
}

fn answers(view: &Entity<ProjectView>, visual: &VisualTestContext) -> BTreeMap<String, Value> {
    view.read_with(visual, |view, cx| {
        let Intent::Respond {
            answers: Some(answers),
            ..
        } = view.native.actions.read(cx).intent(ID, cx)
        else {
            panic!("question answer intent")
        };
        answers
    })
}

fn active(view: &Entity<ProjectView>, visual: &VisualTestContext) -> String {
    view.read_with(visual, |view, cx| {
        view.native.actions.read(cx).question_progress[ID]
            .active()
            .unwrap()
            .into()
    })
}

#[gpui::test]
fn real_digits_from_composer_controls_keep_exact_values_repeat_delay_and_multi_toggles(
    cx: &mut TestAppContext,
) {
    let (view, visual, _temp) = fixture(cx);
    show(&view, visual, request(false));
    assert!(
        answers(&view, visual).is_empty(),
        "no implicit first choice"
    );
    assert!(visual.debug_bounds("answer-key-options-first-1").is_some());
    focus_composer_control(&view, visual);
    key(visual, "2", false);
    assert_eq!(answers(&view, visual)["first"], json!("β second"));
    assert!(visual
        .debug_bounds("answer-check-options-first-1")
        .is_some());
    tick(visual, 199);
    assert_eq!(active(&view, visual), "first");
    key(visual, "2", true);
    assert_eq!(answers(&view, visual)["first"], json!("β second"));
    tick(visual, 199);
    assert_eq!(
        active(&view, visual),
        "first",
        "held repeat restarts the same 200ms path"
    );
    tick(visual, 1);
    assert_eq!(active(&view, visual), "last");
    key(visual, "2", false);
    key(visual, "1", false);
    assert_eq!(
        answers(&view, visual)["last"],
        json!("[\"final β\",\" final one \"]")
    );
    tick(visual, 1_000);
    assert_eq!(active(&view, visual), "last");
    key(visual, "2", false);
    assert_eq!(answers(&view, visual)["last"], json!("[\" final one \"]"));
    view.read_with(visual, |view, cx| {
        let actions = view.native.actions.read(cx);
        assert!(
            actions.question_timers.is_empty(),
            "multi never auto advances"
        );
        assert!(actions.records.is_empty());
        assert_eq!(actions.deferred_dispatches.get(), 0);
    });
}

#[gpui::test]
fn digits_ignore_editable_answers_composer_model_search_and_modified_keys(cx: &mut TestAppContext) {
    let (view, visual, _temp) = fixture(cx);
    show(&view, visual, request(true));
    focus_composer_control(&view, visual);
    logical_key(visual, "shift-1", "!");
    assert!(
        answers(&view, visual).is_empty(),
        "the physical digit key cannot select when it types punctuation"
    );
    logical_key(visual, "shift-&", "1");
    assert_eq!(
        answers(&view, visual)["first"],
        json!("[\" exact first \"]"),
        "a logical digit selects its option even on a shifted keyboard layout"
    );
    logical_key(visual, "1", "1");
    assert!(
        answers(&view, visual).is_empty(),
        "ordinary logical digits use the same multi toggle path"
    );
    for name in ["ctrl-2", "alt-2", "cmd-2", "0", "9"] {
        key(visual, name, false);
        assert!(
            answers(&view, visual).is_empty(),
            "{name} cannot select an option"
        );
    }
    view.update_in(visual, |view, window, cx| {
        view.compose.update(cx, |input, cx| input.focus(window, cx));
    });
    // Deliberately send the first key before run_until_parked or a paint can
    // register the newly focused composer in the kit's focused-input cache.
    key(visual, "2", false);
    assert!(answers(&view, visual).is_empty());
    view.read_with(visual, |view, cx| {
        let actions = view.native.actions.read(cx);
        assert!(actions.question_timers.is_empty());
        assert_eq!(actions.deferred_dispatches.get(), 0);
    });
    focus_composer_control(&view, visual);
    key(visual, "2", false);
    assert_eq!(
        answers(&view, visual)["first"],
        json!("[\"β second\"]"),
        "positive control: the same digit works after focus leaves the composer"
    );
    key(visual, "2", false);
    assert!(answers(&view, visual).is_empty());
    // Both visible custom fields and the ordinary message composer retain digits.
    click(visual, "answer-custom-options-first");
    key(visual, "2", false);
    assert!(answers(&view, visual).is_empty());
    click(visual, "project-composer-input");
    key(visual, "2", false);
    assert!(answers(&view, visual).is_empty());
    view.update_in(visual, |view, window, cx| {
        view.open_models(
            okilum_t3::models::Target {
                thread_id: THREAD.into(),
                project_id: "fixture-managers".into(),
            },
            window,
            cx,
        );
    });
    visual.run_until_parked();
    assert!(
        visual.update(|window, cx| window.has_focused_input(cx)),
        "positive control: model search holds editable focus"
    );
    key(visual, "2", false);
    assert!(answers(&view, visual).is_empty());
    view.update(visual, |view, cx| {
        view.model_open = false;
        cx.notify();
    });
    visual.run_until_parked();
    focus_composer_control(&view, visual);
    key(visual, "2", false);
    assert_eq!(
        answers(&view, visual)["first"],
        json!("[\"β second\"]"),
        "positive control: the same unmodified digit works outside every input"
    );
}

#[gpui::test]
fn digits_reject_collapsed_busy_submitting_unready_retained_and_stale_controls(
    cx: &mut TestAppContext,
) {
    let (view, visual, _temp) = fixture(cx);
    show(&view, visual, request(true));
    click(visual, "question-header-options-first");
    assert!(visual.debug_bounds("answer-key-options-first-0").is_none());
    key(visual, "2", false);
    assert!(answers(&view, visual).is_empty());
    click(visual, "question-header-options-first");
    focus_composer_control(&view, visual);
    for guard in [
        "busy",
        "submitting",
        "not-ready",
        "retained",
        "unpinned",
        "foreign-lease",
        "foreign-conversation",
    ] {
        view.update(visual, |view, cx| {
            view.native.actions.update(cx, |actions, _| match guard {
                "busy" => actions.busy = true,
                "submitting" => actions
                    .question_progress
                    .get_mut(ID)
                    .unwrap()
                    .begin_submission(),
                "not-ready" => actions.state.ready = false,
                "retained" => actions.records.push(Record {
                    command: okilum_t3::actions::Command::new(
                        THREAD,
                        Intent::Dismiss {
                            request_id: ID.into(),
                        },
                    ),
                    sequence: Some(1),
                }),
                "unpinned" => actions.pinned = false,
                "foreign-lease" => actions.lease = Some(Arc::new(())),
                "foreign-conversation" => {
                    actions.conversation = Some(("other-origin".into(), "other-thread".into()))
                }
                _ => unreachable!(),
            });
        });
        key(visual, "2", false);
        assert!(
            answers(&view, visual).is_empty(),
            "{guard} rejects the rendered key binding"
        );
        view.update_in(visual, |view, window, cx| {
            view.native.actions.update(cx, |actions, cx| {
                actions.busy = false;
                actions.state.ready = true;
                actions.pinned = true;
                actions.records.clear();
                actions
                    .question_progress
                    .get_mut(ID)
                    .unwrap()
                    .finish_submission();
                actions.set_scope(
                    view.native_action_conversation(),
                    &view.native.request_lease,
                    cx,
                );
                // A foreign conversation/lease resets state, so rebuild the real surface.
                actions.set_state(
                    State {
                        ready: true,
                        requests: vec![request(true)],
                        ..Default::default()
                    },
                    window,
                    cx,
                );
                actions.set_enabled(true, cx);
                cx.notify();
            });
        });
        visual.run_until_parked();
        focus_composer_control(&view, visual);
        key(visual, "2", false);
        assert_eq!(
            answers(&view, visual)["first"],
            json!("[\"β second\"]"),
            "positive control after {guard}"
        );
        key(visual, "2", false);
        assert!(answers(&view, visual).is_empty());
    }
    // An old rendered key must not bind to changed options with reused IDs.
    let old = view.update(visual, |view, cx| {
        view.native
            .actions
            .update(cx, |actions, _| actions.question_keys.take().unwrap())
    });
    let mut changed = request(true);
    changed.item["questions"][0]["options"][1]["value"] = json!("replacement value");
    show(&view, visual, changed);
    let current = view.update(visual, |view, cx| {
        view.native
            .actions
            .update(cx, |actions, _| actions.question_keys.replace(old).unwrap())
    });
    key(visual, "2", false);
    assert!(
        answers(&view, visual).is_empty(),
        "old schema cannot reinterpret a digit"
    );
    view.update(visual, |view, cx| {
        view.native
            .actions
            .update(cx, |actions, _| actions.question_keys = Some(current))
    });
    key(visual, "2", false);
    assert_eq!(
        answers(&view, visual)["first"],
        json!("[\"replacement value\"]")
    );
}

#[gpui::test]
fn narrow_options_show_wrapped_label_distinct_description_check_and_multi_guidance(
    cx: &mut TestAppContext,
) {
    let (view, visual, _temp) = fixture(cx);
    let mut options = request(true);
    options.item["questions"] = json!([{"id":"first","header":"Choose","question":"Choose a workflow","multiSelect":true,"allowCustomAnswer":false,
        "options":[{"label":"Keep the existing workflow and preserve the current review history",
                    "description":"Continue with the established review sequence while keeping every existing draft and decision available.", "value":" exact "},
                   {"label":"Start fresh","description":"Start fresh","value":"fresh"}]}]);
    visual.simulate_resize(size(px(360.), px(1000.)));
    show(&view, visual, options);
    assert!(visual
        .debug_bounds("question-multi-guidance-options")
        .is_some());
    let button = visual.debug_bounds("answer-options-first-0").unwrap();
    let label = visual.debug_bounds("answer-label-options-first-0").unwrap();
    let description = visual
        .debug_bounds("answer-description-options-first-0")
        .unwrap();
    assert!(
        label.size.height > px(20.),
        "long labels wrap instead of truncating"
    );
    assert!(
        description.size.height > px(16.),
        "description is visible multiline text before hover"
    );
    assert!(description.top() >= label.bottom());
    assert!(label.left() >= button.left() && label.right() <= button.right());
    assert!(description.left() >= button.left() && description.right() <= button.right());
    assert!(
        description.bottom() <= button.bottom(),
        "the button grows around its full text"
    );
    assert!(
        visual
            .debug_bounds("answer-description-options-first-1")
            .is_none(),
        "identical description is not repeated"
    );
    click(visual, "answer-options-first-0");
    assert!(visual
        .debug_bounds("answer-check-options-first-0")
        .is_some());
    assert!(visual.debug_bounds("answer-key-options-first-0").is_none());
    assert_eq!(answers(&view, visual)["first"], json!("[\" exact \"]"));
    tick(visual, 1_000);
    view.read_with(visual, |view, cx| {
        assert!(view.native.actions.read(cx).records.is_empty())
    });
}

#[gpui::test]
fn digits_keep_rendered_option_ordinals_and_only_the_first_nine_have_hints(
    cx: &mut TestAppContext,
) {
    let (view, visual, _temp) = fixture(cx);
    let mut choices = request(true);
    choices.item["questions"][0]["allowCustomAnswer"] = json!(false);
    choices.item["questions"][0]["options"] = json!((1..=10)
        .map(|n| json!({"label":format!("Choice {n}"),"value":format!(" exact {n} ")}))
        .collect::<Vec<_>>());
    show(&view, visual, choices);
    focus_composer_control(&view, visual);
    key(visual, "9", false);
    assert_eq!(
        answers(&view, visual)["first"],
        json!("[\" exact 9 \"]"),
        "expanded options retain their fixed ordinal even below the panel scroll"
    );
    key(visual, "9", false);
    assert!(answers(&view, visual).is_empty());
    let panel = visual.debug_bounds("project-native-actions").unwrap();
    visual.simulate_event(gpui::ScrollWheelEvent {
        position: panel.center(),
        delta: gpui::ScrollDelta::Pixels(point(px(0.), px(-3_000.))),
        ..Default::default()
    });
    visual.run_until_parked();
    assert!(
        visual
            .debug_bounds("answer-label-options-first-9")
            .is_some(),
        "positive control: the tenth option is painted after scrolling"
    );
    assert!(visual.debug_bounds("answer-key-options-first-8").is_some());
    assert!(
        visual.debug_bounds("answer-key-options-first-9").is_none(),
        "the tenth option has no invented digit shortcut"
    );
    key(visual, "1", false);
    assert_eq!(
        answers(&view, visual)["first"],
        json!("[\" exact 1 \"]"),
        "scrolling does not renumber the active question's choices"
    );
}
