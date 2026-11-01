//! Complete approval context and provider decisions; presentation never grants authority.
use super::*;

fn approval_kind(request: &Request) -> (&'static str, &'static str) {
    if request.approval_detail.is_some() {
        return ("Command approval", "Command");
    }
    match request.item["requestKind"].as_str() {
        Some("command") => ("Command approval", "Command"),
        Some("file-read") => ("File read approval", "File to read"),
        Some("file-change") => ("File change approval", "File change"),
        Some("mcp-elicitation") => ("App access approval", "App access request"),
        // Native permission requests and future kinds must not be described as
        // a file operation merely because the web client's fallback does so.
        _ => ("Approval requested", "Approval detail"),
    }
}

fn approval_detail(request: &Request) -> String {
    if request.approval_detail.is_some() || request.item["requestKind"] == "command" {
        return request
            .approval_detail
            .as_deref()
            .filter(|detail| !detail.is_empty())
            .unwrap_or("Command detail unavailable")
            .to_owned();
    }
    request.item["prompt"]
        .as_str()
        .filter(|text| !text.is_empty())
        .or_else(|| {
            request.item["reason"]
                .as_str()
                .filter(|text| !text.is_empty())
        })
        .unwrap_or(approval_kind(request).0)
        .to_owned()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DecisionTone {
    Muted,
    Approve,
    Decline,
    Warning,
}

fn decision_tone(decision: &str, warning: bool) -> DecisionTone {
    // Keep T3's precedence when the provider adds a caution to Approve/Decline.
    match decision {
        "decline" => DecisionTone::Decline,
        "accept" => DecisionTone::Approve,
        _ if warning => DecisionTone::Warning,
        _ => DecisionTone::Muted,
    }
}

struct ApprovalControl {
    conversation: Option<(String, String)>,
    lease: Option<Arc<()>>,
}

impl ApprovalControl {
    fn current(&self, conversation: &Option<(String, String)>, lease: Option<&Arc<()>>) -> bool {
        &self.conversation == conversation
            && self
                .lease
                .as_ref()
                .zip(lease)
                .is_some_and(|(rendered, current)| Arc::ptr_eq(rendered, current))
    }
}

struct TextState {
    focus: gpui::FocusHandle,
    scroll: gpui::ScrollHandle,
}

#[derive(IntoElement)]
struct ApprovalText {
    id: SharedString,
    selector: String,
    label: String,
    text: String,
    code: bool,
}

impl RenderOnce for ApprovalText {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(self.id.clone(), cx, |_, cx| TextState {
            // GPUI registers the tracked handle's tab-stop flag; setting the
            // element's flag does not change an explicit tracked handle.
            focus: cx.focus_handle().tab_stop(true),
            scroll: gpui::ScrollHandle::new(),
        });
        let focus = state.read(cx).focus.clone();
        let scroll = state.read(cx).scroll.clone();
        let keyboard_scroll = scroll.clone();
        let click_focus = focus.clone();
        let selector = self.selector;
        let mut content = div()
            .id(self.id)
            .debug_selector({
                let selector = selector.clone();
                move || selector.clone()
            })
            .key_context("ProjectApprovalDetail")
            .track_focus(&focus)
            .aria_label(self.label)
            .aria_description(self.text.clone())
            .w_full()
            .min_w_0()
            .max_h(px(80.))
            .overflow_scroll()
            .track_scroll(&scroll)
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .p_1()
            .text_xs()
            .line_height(px(16.))
            .focus_visible(|style| style.border_color(cx.theme().ring))
            .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                click_focus.focus(window, cx);
            })
            .on_key_down(move |event, window, cx| {
                let modifiers = event.keystroke.modifiers;
                if modifiers.control || modifiers.alt || modifiers.platform {
                    return;
                }
                let mut offset = keyboard_scroll.offset();
                match event.keystroke.key.as_str() {
                    "up" => offset.y += px(16.),
                    "down" => offset.y -= px(16.),
                    "left" => offset.x += px(48.),
                    "right" => offset.x -= px(48.),
                    "pageup" => offset.y += px(64.),
                    "pagedown" => offset.y -= px(64.),
                    "home" => offset = point(px(0.), px(0.)),
                    "end" => {
                        let maximum = keyboard_scroll.max_offset();
                        offset = point(-maximum.x, -maximum.y);
                    }
                    _ => return,
                }
                keyboard_scroll.set_offset(offset);
                window.refresh();
                cx.stop_propagation();
            });
        if self.code {
            let family = crate::reader_fonts::code(cx);
            let mut font = window.text_style().font();
            font.family = family.clone();
            let lines = self.text.split('\n').collect::<Vec<_>>();
            let width = lines.iter().fold(px(1.), |width, text| {
                let run = gpui::TextRun {
                    len: text.len(),
                    font: font.clone(),
                    color: cx.theme().foreground,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                width.max(
                    window
                        .text_system()
                        .shape_line(
                            (*text).to_owned().into(),
                            window.rem_size() * 0.75,
                            &[run],
                            None,
                        )
                        .width()
                        .ceil(),
                )
            });
            content = content.font_family(family);
            // Explicit unwrapped glyph width gives the scroll container a
            // complete horizontal extent. A max-content grid track alone may
            // overflow a viewport-sized grid without widening that extent.
            content = content.child(v_flex().w(width).min_w_full().children(
                lines.into_iter().enumerate().map(|(line, text)| {
                    let text_selector = format!("{selector}-line-{line}");
                    div()
                        .debug_selector(move || text_selector.clone())
                        .w_full()
                        .min_h(px(16.))
                        .whitespace_nowrap()
                        .child(text.to_owned())
                }),
            ));
        } else {
            content = content.child(div().whitespace_normal().child(self.text));
        }
        content
    }
}

struct WarningFocus {
    focus: gpui::FocusHandle,
    _subscriptions: [Subscription; 2],
}

#[derive(IntoElement)]
struct ApprovalDecision {
    scope: String,
    selector: String,
    button: Button,
    warning: Option<String>,
    disabled: bool,
}

impl RenderOnce for ApprovalDecision {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Some(warning) = self.warning else {
            return div().child(self.button).into_any_element();
        };
        let state = window.use_keyed_state(
            SharedString::from(format!("approval-warning-focus-{}", self.scope)),
            cx,
            |window, cx| {
                let focus = cx.focus_handle();
                let entered = window.on_focus_in(&focus, cx, |window, _| window.refresh());
                let left = window.on_focus_out(&focus, cx, |_, window, _| window.refresh());
                WarningFocus {
                    focus,
                    _subscriptions: [entered, left],
                }
            },
        );
        // Disabled decisions keep a read-only keyboard target for the caution.
        let focus = state.read(cx).focus.clone().tab_stop(self.disabled);
        let focused = focus.contains_focused(window, cx);
        let selector = format!("{}-warning", self.selector);
        let mut group = v_flex()
            .id(SharedString::from(format!(
                "approval-warning-{}",
                self.scope
            )))
            .key_context("ProjectApprovalDecision")
            .track_focus(&focus)
            .aria_label("Approval warning")
            .aria_description(warning.clone())
            .min_w_0()
            .max_w_full()
            .gap_1()
            .child(self.button.tooltip(warning.clone()));
        if focused {
            group = group.child(ApprovalText {
                id: format!("approval-warning-text-{}", self.scope).into(),
                selector,
                label: "Provider warning".into(),
                text: warning,
                code: false,
            });
        }
        group.into_any_element()
    }
}

impl Actions {
    pub(super) fn render_approval(
        &mut self,
        request: &Request,
        disabled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let request_id = &request.id;
        let (kind, detail_label) = approval_kind(request);
        let detail = approval_detail(request);
        let scope = format!("{:?}:{request_id}", self.conversation);
        let mut card = v_flex().w_full().min_w_0().gap_1().child(
            div()
                .debug_selector({
                    let request = request_id.clone();
                    move || format!("approval-kind-{request}")
                })
                .text_xs()
                .text_color(brand::palette(cx).text_muted)
                .child(kind),
        );
        if let Some(app) = request.item["appName"]
            .as_str()
            .filter(|app| !app.is_empty())
        {
            card = card.child(
                div()
                    .debug_selector({
                        let request = request_id.clone();
                        move || format!("approval-app-{request}")
                    })
                    .whitespace_normal()
                    .child(app.to_owned()),
            );
        }
        card = card.child(ApprovalText {
            id: format!("approval-detail-{scope}").into(),
            selector: format!("approval-detail-{request_id}"),
            label: if request.item["requestKind"] == "command" && request.approval_detail.is_none()
            {
                "Command detail unavailable".into()
            } else {
                detail_label.into()
            },
            text: detail.clone(),
            code: true,
        });
        for field in ["prompt", "reason"] {
            if let Some(text) = request.item[field].as_str().filter(|text| {
                !text.is_empty()
                    && *text != detail
                    && (field != "reason" || Some(*text) != request.item["prompt"].as_str())
            }) {
                card = card.child(
                    div()
                        .debug_selector({
                            let request = request_id.clone();
                            move || format!("approval-provider-{field}-{request}")
                        })
                        .whitespace_normal()
                        .child(text.to_owned()),
                );
            }
        }
        if let Some(reason) = request.item["fullAccessOverride"].as_str() {
            card = card.child(match reason {
                "ask_rule" => "Your approval rule requires this decision.",
                "provider_check" => "The provider requires this decision.",
                _ => "This action requires an additional approval.",
            });
        }
        let options = match request.item.get("options") {
            None => APPROVAL_FALLBACK_OPTIONS
                .iter()
                .map(|(decision, label)| json!({"decision":decision,"label":label}))
                .collect(),
            Some(options) => options.as_array().cloned().unwrap_or_default(),
        };
        let mut buttons = h_flex()
            .w_full()
            .min_w_0()
            .items_start()
            .flex_wrap()
            .gap_1();
        for (n, option) in options.iter().enumerate() {
            let Some(decision) = option["decision"].as_str() else {
                continue;
            };
            let intent = Intent::Respond {
                request_id: request_id.clone(),
                decision: Some(decision.into()),
                answers: None,
                attachments_by_question_id: Default::default(),
            };
            let allowed = self.state.allows(&intent);
            let response_request = request_id.clone();
            let control = ApprovalControl {
                conversation: self.conversation.clone(),
                lease: self.lease.clone(),
            };
            let warning = option["warning"]
                .as_str()
                .filter(|warning| !warning.is_empty())
                .map(str::to_owned);
            let tone = decision_tone(decision, warning.is_some());
            let label = option["label"].as_str().unwrap_or(decision).to_owned();
            let selector = format!("approval-{request_id}-{n}");
            let mut button = Button::new(SharedString::from(selector.clone()))
                .debug_selector({
                    let selector = selector.clone();
                    move || selector.clone()
                })
                .ghost()
                .label(label)
                .text_color(match tone {
                    DecisionTone::Muted => cx.theme().muted_foreground,
                    DecisionTone::Approve => cx.theme().foreground,
                    DecisionTone::Decline => cx.theme().danger,
                    DecisionTone::Warning => cx.theme().warning,
                })
                .disabled(disabled || !allowed)
                .on_click(cx.listener(move |this, _, window, cx| {
                    if control.current(&this.conversation, this.lease.as_ref())
                        && this.enabled
                        && !this.busy
                        && !this.request_retained(&response_request)
                        && this.state.allows(&intent)
                    {
                        this.dispatch(intent.clone(), window, cx);
                    }
                }));
            if warning.is_some() {
                button = button.icon(IconName::TriangleAlert);
            }
            buttons = buttons.child(ApprovalDecision {
                scope: format!("{scope}:{n}"),
                selector,
                button,
                warning,
                disabled: disabled || !allowed,
            });
        }
        card.child(buttons).into_any_element()
    }
}

#[cfg(test)]
mod tests;
