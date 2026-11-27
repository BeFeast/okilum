use super::*;
use ::core::prelude::v1::test;
use std::{cell::RefCell, rc::Rc};

fn request(item: serde_json::Value) -> Request {
    Request {
        id: "derived".into(),
        item,
        answerable: true,
        dismissible: false,
        response_mode: Some("live".into()),
        reason: None,
        approval_detail: None,
    }
}

#[test]
fn context_uses_complete_native_fields_and_truthful_kind_fallbacks() {
    let detail = "  command --literal='α'\n\n    LAST_SENTINEL  ";
    for (kind, label, aria) in [
        ("command", "Command approval", "Command"),
        ("file-read", "File read approval", "File to read"),
        ("file-change", "File change approval", "File change"),
        (
            "mcp-elicitation",
            "App access approval",
            "App access request",
        ),
        ("permission", "Approval requested", "Approval detail"),
        ("future-kind", "Approval requested", "Approval detail"),
    ] {
        let mut request =
            request(json!({"requestKind":kind,"prompt":detail,"reason":"distinct reason"}));
        assert_eq!(approval_kind(&request), (label, aria));
        if kind == "command" {
            assert_eq!(approval_detail(&request), "Command detail unavailable");
            request.approval_detail = Some(detail.into());
            request.item["prompt"] = json!("The provider supplied only a justification");
            assert_eq!(approval_detail(&request), detail);
        } else {
            assert_eq!(approval_detail(&request), detail);
            request.item["prompt"] = json!("");
            assert_eq!(approval_detail(&request), "distinct reason");
            request.item["reason"] = serde_json::Value::Null;
            assert_eq!(approval_detail(&request), label);
        }
    }
}

fn command_state(command: &str, has_kind: bool) -> State {
    use okilum_t3::{projection::Projection, stream::ThreadUpdate};
    let reference = json!({"driver":"codex","nativeId":"exact-command","strength":"strong"});
    let mut approval = json!({"id":"approval-row","threadId":"thread","runId":"run",
        "nodeId":"approval-node","providerThreadId":"provider-thread","providerTurnId":"provider-turn",
        "nativeItemRef":reference,"ordinal":2,"status":"pending","type":"approval_request",
        "requestId":"derived","requestKind":"command","prompt":"Justification is not the command",
        "reason":"Distinct provider reason","appName":"Fixture app"});
    if !has_kind {
        approval.as_object_mut().unwrap().remove("requestKind");
    }
    let execution = json!({"id":"execution-row","threadId":"thread","runId":"run",
        "nodeId":"execution-node","providerThreadId":"provider-thread","providerTurnId":"provider-turn",
        "nativeItemRef":reference,"ordinal":1,"status":"running","type":"command_execution",
        "input":command});
    let items = vec![execution, approval];
    let snapshot = ThreadUpdate::Snapshot {
        sequence: 1,
        projection: json!({
            "thread":{"id":"thread","projectId":"project","title":"Approval fixture"},
            "runs":[{"id":"run","ordinal":1,"status":"waiting","providerThreadId":"provider-thread"}],
            "attempts":[],"providerThreads":[{"id":"provider-thread"}],"providerSessions":[],
            "runtimeRequests":[{"id":"derived","threadId":"thread","runId":"run","nodeId":"approval-node",
                "providerThreadId":"provider-thread","providerTurnId":"provider-turn",
                "nativeRequestRef":reference,"kind":"command","status":"pending","responseCapability":{"type":"live"}}],
            "turnItems":items,
            "visibleTurnItems":items.iter().enumerate().map(|(position,item)|json!({
                "position":position,"visibility":"local","sourceThreadId":"thread",
                "sourceItemId":item["id"],"item":item})).collect::<Vec<_>>()
        }),
        history_cursor: None,
        has_more_history: false,
        latest_local_turn_ordinal: None,
    };
    let mut projection = Projection::new("thread", "project");
    projection.apply(&[snapshot]).unwrap();
    projection.action_state()
}

struct ContextHarness {
    _parent: Entity<ProjectView>,
    actions: Entity<Actions>,
    request: Request,
}
impl Render for ContextHarness {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let request = self.request.clone();
        v_flex()
            .size_full()
            .min_w_0()
            .p_2()
            .child(self.actions.update(cx, |actions, cx| {
                actions.render_approval(&request, false, cx)
            }))
    }
}

#[gpui::test]
fn derived_full_command_is_rendered_separately_from_provider_justification(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    cx.update(crate::test_init);
    let command = (0..20)
        .map(|n| format!("  command {n:02} {} CANONICAL_TAIL", "x".repeat(400)))
        .collect::<Vec<_>>()
        .join("\n");
    let state = command_state(&command, true);
    let request = state.requests.first().unwrap().clone();
    assert_eq!(request.approval_detail.as_deref(), Some(command.as_str()));
    assert_eq!(request.item["prompt"], "Justification is not the command");
    let temp = tempfile::tempdir().unwrap();
    let (root, visual) = cx.add_window_view(|window, cx| {
        let parent = cx.new(|cx| {
            let mut parent = crate::project::fixture::view(
                temp.path().to_owned(),
                Arc::new(crate::project::fixture::Fake::default()),
                window,
                cx,
            );
            parent.active = false;
            super::super::tests::inert_native(&mut parent);
            parent
        });
        let actions = parent.read(cx).native.actions.clone();
        actions.update(cx, |actions, cx| {
            actions.set_state(state, window, cx);
            actions.set_enabled(true, cx);
        });
        let host = cx.new(|_| ContextHarness {
            _parent: parent,
            actions,
            request,
        });
        Root::new(host, window, cx)
    });
    visual.simulate_resize(size(px(420.), px(600.)));
    visual.run_until_parked();
    let viewport = visual.debug_bounds("approval-detail-derived").unwrap();
    let first = visual
        .debug_bounds("approval-detail-derived-line-0")
        .unwrap();
    let last = visual
        .debug_bounds("approval-detail-derived-line-19")
        .unwrap();
    assert!(first.size.width > viewport.size.width * 2.);
    assert!(last.origin.y > viewport.bottom());
    assert!(visual
        .debug_bounds("approval-provider-prompt-derived")
        .is_some());
    assert!(visual
        .debug_bounds("approval-provider-reason-derived")
        .is_some());
    assert!(visual.debug_bounds("approval-app-derived").is_some());
    for selector in [
        "approval-derived-0",
        "approval-derived-1",
        "approval-derived-2",
        "approval-derived-3",
    ] {
        assert!(visual.debug_bounds(selector).is_some());
    }
    // Missing/ambiguous context must stay honest without changing authority.
    let host = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<ContextHarness>().unwrap()
    });
    let without_kind = command_state(&command, false).requests.remove(0);
    assert_eq!(
        without_kind.approval_detail.as_deref(),
        Some(command.as_str())
    );
    assert!(without_kind.item.get("requestKind").is_none());
    assert_eq!(
        approval_kind(&without_kind),
        ("Command approval", "Command")
    );
    host.update(visual, |host, cx| {
        host.request = without_kind;
        cx.notify();
    });
    visual.run_until_parked();
    assert!(visual
        .debug_bounds("approval-detail-derived-line-19")
        .is_some());
    assert!(visual
        .debug_bounds("approval-provider-prompt-derived")
        .is_some());
    host.update(visual, |host, cx| {
        host.request.item["requestKind"] = json!("command");
        host.request.approval_detail = None;
        assert_eq!(approval_detail(&host.request), "Command detail unavailable");
        let mut authority = host.actions.read(cx).state.clone();
        authority.requests[0].approval_detail = None;
        assert!(authority.allows(&Intent::Respond {
            request_id: "derived".into(),
            decision: Some("accept".into()),
            answers: None,
            attachments_by_question_id: Default::default(),
        }));
        cx.notify();
    });
    visual.run_until_parked();
    assert!(visual
        .debug_bounds("approval-detail-derived-line-0")
        .is_some());
    assert!(visual
        .debug_bounds("approval-detail-derived-line-19")
        .is_none());
    assert!(visual
        .debug_bounds("approval-provider-prompt-derived")
        .is_some());
    for selector in [
        "approval-derived-0",
        "approval-derived-1",
        "approval-derived-2",
        "approval-derived-3",
    ] {
        assert!(visual.debug_bounds(selector).is_some());
    }
}

#[test]
fn provider_warning_does_not_override_approve_or_decline_semantics() {
    for warning in [false, true] {
        assert_eq!(decision_tone("accept", warning), DecisionTone::Approve);
        assert_eq!(decision_tone("decline", warning), DecisionTone::Decline);
    }
    for decision in ["cancel", "acceptForSession", "acceptAlways"] {
        assert_eq!(decision_tone(decision, true), DecisionTone::Warning);
        assert_eq!(decision_tone(decision, false), DecisionTone::Muted);
    }
}

#[test]
fn rendered_approval_control_rejects_replacement_conversation_or_lease() {
    let scope = Some(("https://origin.example".into(), "thread".into()));
    let lease = Arc::new(());
    let control = ApprovalControl {
        conversation: scope.clone(),
        lease: Some(lease.clone()),
    };
    assert!(control.current(&scope, Some(&lease)));
    assert!(!control.current(
        &Some(("https://other.example".into(), "thread".into())),
        Some(&lease)
    ));
    assert!(!control.current(
        &Some(("https://origin.example".into(), "replacement".into())),
        Some(&lease)
    ));
    assert!(!control.current(&scope, Some(&Arc::new(()))));
    assert!(!control.current(&scope, None));
    assert!(!control.current(&None, Some(&lease)));
    assert!(control.current(&scope, Some(&lease)));
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

struct TextHarness(String);
impl Render for TextHarness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        v_flex().size_full().p_2().child(ApprovalText {
            id: "approval-scroll-fixture".into(),
            selector: "approval-scroll-fixture".into(),
            label: "Command".into(),
            text: self.0.clone(),
            code: true,
        })
    }
}

#[gpui::test]
fn complete_command_detail_is_keyboard_focusable_and_scrolls_both_axes(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(crate::test_init);
    let text = (0..30)
        .map(|n| {
            format!(
                "  command {n:02} {} END_OF_LINE_{n:02}",
                "abcdefghij".repeat(30)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|_| TextHarness(text));
        Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(360.), px(400.)));
    visual.run_until_parked();
    let viewport = visual.debug_bounds("approval-scroll-fixture").unwrap();
    let first = visual
        .debug_bounds("approval-scroll-fixture-line-0")
        .unwrap();
    let last = visual
        .debug_bounds("approval-scroll-fixture-line-29")
        .unwrap();
    assert!(viewport.size.height <= px(80.));
    assert!(
        first.size.width > viewport.size.width * 2.,
        "positive control: full long command has horizontal extent; viewport={viewport:?}, first={first:?}"
    );
    assert!(
        last.origin.y > viewport.bottom(),
        "positive control: full multiline command has vertical extent; viewport={viewport:?}, last={last:?}"
    );
    visual.simulate_click(viewport.center(), Modifiers::default());
    visual.run_until_parked();
    visual.update(|window, cx| {
        assert!(window.focused(cx).is_some());
        assert!(window
            .context_stack()
            .iter()
            .any(|context| context.contains("ProjectApprovalDetail")));
    });
    key(visual, "pagedown");
    let paged = visual
        .debug_bounds("approval-scroll-fixture-line-0")
        .unwrap();
    assert!(
        paged.origin.y < first.origin.y - px(30.),
        "PageDown scrolls actual text; before={first:?}, after={paged:?}"
    );
    key(visual, "right");
    let right = visual
        .debug_bounds("approval-scroll-fixture-line-0")
        .unwrap();
    assert!(
        right.origin.x < first.origin.x - px(20.),
        "Right scrolls actual text; before={first:?}, after={right:?}"
    );
    key(visual, "end");
    let end = visual
        .debug_bounds("approval-scroll-fixture-line-29")
        .unwrap();
    assert!(end.bottom() <= viewport.bottom());
    assert!(end.right() <= viewport.right());
    key(visual, "home");
    assert_eq!(
        visual
            .debug_bounds("approval-scroll-fixture-line-0")
            .unwrap(),
        first,
        "the complete original command remains available after both-axis scrolling"
    );
}

struct WarningHarness {
    anchor: FocusHandle,
    disabled: bool,
    decisions: Rc<RefCell<Vec<String>>>,
}
impl Render for WarningHarness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let decisions = self.decisions.clone();
        v_flex()
            .size_full()
            .tab_group()
            .p_2()
            .child(
                div()
                    .track_focus(&self.anchor)
                    .child("Keyboard focus start"),
            )
            .child(ApprovalDecision {
                scope: "warning-focus-fixture".into(),
                selector: "warning-choice".into(),
                button: Button::new("warning-choice")
                    .debug_selector(|| "warning-choice".into())
                    .ghost()
                    .icon(IconName::TriangleAlert)
                    .label("Provider's exact always label")
                    .disabled(self.disabled)
                    .on_click(move |_, _, _| decisions.borrow_mut().push("acceptAlways".into())),
                warning: Some("Full provider warning\nincluding the final WARNING_SENTINEL".into()),
                disabled: self.disabled,
            })
            .child(
                div()
                    .key_context("FollowingApprovalControl")
                    .child(Button::new("following-control").label("Following control")),
            )
    }
}

#[gpui::test]
fn keyboard_focus_exposes_full_provider_warning_without_activating_the_decision(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    cx.update(crate::test_init);
    let decisions = Rc::new(RefCell::new(Vec::new()));
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let anchor = cx.focus_handle().tab_stop(true);
            anchor.focus(window, cx);
            WarningHarness {
                anchor,
                disabled: false,
                decisions: decisions.clone(),
            }
        });
        Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(360.), px(400.)));
    visual.run_until_parked();
    assert!(visual.debug_bounds("warning-choice").is_some());
    assert!(visual.debug_bounds("warning-choice-warning").is_none());
    key(visual, "tab");
    visual.update(|window, cx| {
        assert!(
            window.focused(cx).is_some(),
            "positive control: keyboard focus exists"
        );
        assert!(
            window
                .context_stack()
                .iter()
                .any(|context| context.contains("ProjectApprovalDecision")),
            "positive control: actual Tab reached the provider decision; focus={:?}, contexts={:?}",
            window.focused(cx),
            window.context_stack()
        );
    });
    assert!(
        visual.debug_bounds("warning-choice-warning").is_some(),
        "the full warning is rendered on actual option focus, without hover or activation"
    );
    assert!(decisions.borrow().is_empty());
    key(visual, "enter");
    assert_eq!(&*decisions.borrow(), &["acceptAlways"]);
    key(visual, "tab");
    visual.update(|window, cx| {
        assert!(
            window
                .context_stack()
                .iter()
                .any(|context| context.contains("ProjectApprovalDetail")),
            "positive control: actual Tab entered the focusable full warning; focus={:?}, contexts={:?}",
            window.focused(cx),
            window.context_stack()
        );
    });
    assert!(
        visual.debug_bounds("warning-choice-warning").is_some(),
        "the full warning remains available while its text area has focus"
    );
    key(visual, "tab");
    visual.update(|window, cx| {
        assert!(
            window
                .context_stack()
                .iter()
                .any(|context| context.contains("FollowingApprovalControl")),
            "positive control: actual Tab left the warning for the following control; focus={:?}, contexts={:?}",
            window.focused(cx),
            window.context_stack()
        );
    });
    assert!(
        visual.debug_bounds("warning-choice-warning").is_none(),
        "leaving the option and its details restores the compact row"
    );
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<WarningHarness>().unwrap()
    });
    view.update(visual, |view, cx| {
        view.disabled = true;
        cx.notify();
    });
    visual.run_until_parked();
    key(visual, "shift-tab");
    assert!(
        visual.debug_bounds("warning-choice-warning").is_some(),
        "a disabled option still exposes its caution through a read-only focus target"
    );
    let button = visual.debug_bounds("warning-choice").unwrap();
    visual.simulate_click(button.center(), Modifiers::default());
    key(visual, "enter");
    assert_eq!(
        &*decisions.borrow(),
        &["acceptAlways"],
        "disabled pointer and keyboard input cannot respond again"
    );
}
