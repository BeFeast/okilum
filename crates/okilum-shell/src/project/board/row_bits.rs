//! Small theme-aware row primitives shared with the compact kanban presentation.
use super::*;

pub(super) fn type_tag(issue: Option<&okilum_t3::board::Issue>, cx: &App) -> AnyElement {
    let (kind, _) = appearance::kind(issue);
    let tone = appearance::type_color(kind, cx);
    div()
        .flex_none()
        .px(px(7.))
        .py(px(1.))
        .rounded(px(5.))
        .text_size(px(12.))
        .line_height(px(16.))
        .text_color(tone)
        .bg(appearance::tint(tone))
        .child(if kind == "fr" { "FR" } else { kind })
        .into_any_element()
}
pub(super) fn label_pill(label: impl Into<SharedString>, tone: Hsla, cx: &App) -> AnyElement {
    h_flex()
        .flex_none()
        .gap(px(5.))
        .px(px(7.))
        .py(px(1.))
        .border_1()
        .border_color(brand::palette(cx).border_subtle)
        .rounded_full()
        .text_size(px(12.))
        .line_height(px(16.))
        .text_color(brand::palette(cx).text_muted)
        .child(div().size(px(6.)).rounded_full().bg(tone))
        .child(label.into())
        .into_any_element()
}
pub(super) fn ci_label(state: Option<&str>) -> &'static str {
    match state {
        Some("success") => "CI passed",
        Some("pending" | "running") => "CI running",
        Some("failure" | "error") => "CI failed",
        _ => "CI unknown",
    }
}
pub(super) fn ci_dot(state: Option<&str>, cx: &App) -> AnyElement {
    let p = brand::palette(cx);
    let tone = match state {
        Some("success") => p.success,
        Some("pending" | "running") => p.warning,
        Some("failure" | "error") => p.danger,
        _ => p.text_muted,
    };
    let label = ci_label(state);
    div()
        .debug_selector(move || label.into())
        .size(px(7.))
        .flex_none()
        .rounded_full()
        .bg(tone)
        .into_any_element()
}
pub(super) fn agent_avatar(name: &str, active: bool, cx: &App) -> AnyElement {
    let p = brand::palette(cx);
    div()
        .relative()
        .flex_none()
        .size(px(18.))
        .rounded_full()
        .bg(appearance::tint(p.info))
        .text_color(p.info)
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(12.))
        .line_height(px(16.))
        .child(
            name.chars()
                .next()
                .unwrap_or('?')
                .to_uppercase()
                .to_string(),
        )
        .when(active, |avatar| {
            avatar.child(
                div()
                    .debug_selector(|| "board-agent-live-dot".into())
                    .absolute()
                    .right(px(-2.))
                    .bottom(px(-2.))
                    .size(px(7.))
                    .rounded_full()
                    .border_1()
                    .border_color(p.canvas)
                    .bg(p.success),
            )
        })
        .into_any_element()
}
pub(super) fn relative_time(timestamp: &str, at: u64) -> Option<String> {
    let updated = chrono::DateTime::parse_from_rfc3339(timestamp)
        .ok()?
        .timestamp();
    let age = at.checked_sub(u64::try_from(updated).ok()?)?;
    Some(if age < 60 {
        "сейчас".into()
    } else if age < 3600 {
        format!("{} мин", age / 60)
    } else if age < 86400 {
        format!("{} ч", age / 3600)
    } else {
        format!("{} д", age / 86400)
    })
}

/// A live dot needs a current unsettled run, not just an assigned agent.
pub(super) fn active(thread: &okilum_t3::ThreadSummary) -> bool {
    matches!(
        thread.status.as_str(),
        "preparing" | "starting" | "running" | "waiting"
    ) && thread
        .active_run_id
        .as_ref()
        .is_some_and(|id| !id.is_empty())
        && !thread.settled
}
