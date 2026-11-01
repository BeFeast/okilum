use super::*;
use ::core::prelude::v1::test;
use std::{cell::RefCell, rc::Rc};

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
        let mut item = json!({"requestKind":kind,"prompt":detail,"reason":"distinct reason"});
        assert_eq!(approval_kind(&item), (label, aria));
        assert_eq!(approval_detail(&item), detail);
        item["prompt"] = json!("");
        assert_eq!(approval_detail(&item), "distinct reason");
        item["reason"] = serde_json::Value::Null;
        assert_eq!(approval_detail(&item), label);
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
    assert!(first.size.width > viewport.size.width * 2.);
    assert!(last.origin.y > viewport.bottom());
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
    assert!(paged.origin.y < first.origin.y - px(30.));
    key(visual, "right");
    let right = visual
        .debug_bounds("approval-scroll-fixture-line-0")
        .unwrap();
    assert!(right.origin.x < first.origin.x - px(20.));
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
            .child(Button::new("following-control").label("Following control"))
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
        let view = cx.new(|_| WarningHarness {
            disabled: false,
            decisions: decisions.clone(),
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
    });
    assert!(
        visual.debug_bounds("warning-choice-warning").is_some(),
        "the full warning is rendered on actual option focus, without hover or activation"
    );
    assert!(decisions.borrow().is_empty());
    key(visual, "enter");
    assert_eq!(&*decisions.borrow(), &["acceptAlways"]);
    key(visual, "tab");
    assert!(
        visual.debug_bounds("warning-choice-warning").is_some(),
        "the full warning remains available while its text area has focus"
    );
    key(visual, "tab");
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
