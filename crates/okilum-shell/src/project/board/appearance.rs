//! Native Board roles reuse the active application's light/dark theme tokens.
use super::*;

pub(super) fn icon(name: &str, color: Hsla) -> gpui::Svg {
    gpui::svg()
        .path(okilum_shell::lucide::asset_path(name))
        .size(px(14.))
        .text_color(color)
}

pub(super) fn kind(issue: Option<&okilum_t3::board::Issue>) -> (&'static str, &'static str) {
    if issue.is_some_and(|i| i.has_label("bug")) {
        ("bug", "bug")
    } else if issue.is_some_and(|i| i.has_label("fr")) {
        ("fr", "sparkles")
    } else if issue.is_some_and(|i| i.has_label("task")) {
        ("task", "check")
    } else {
        ("issue", "circle")
    }
}

pub(super) fn origin(issue: Option<&okilum_t3::board::Issue>) -> Option<&'static str> {
    let issue = issue?;
    match (issue.has_label("from-oleg"), issue.has_label("from-tester")) {
        (true, true) => Some("Oleg / tester"),
        (true, false) => Some("from Oleg"),
        (false, true) => Some("from tester"),
        _ => None,
    }
}

pub(super) fn type_color(kind: &str, cx: &App) -> Hsla {
    let p = brand::palette(cx);
    match kind {
        "bug" => p.danger,
        "fr" => brand::reader_palette(cx).callout_important,
        _ => p.text_muted,
    }
}

pub(super) fn stage(column: Column, cx: &App) -> (Hsla, &'static str) {
    let p = brand::palette(cx);
    match column {
        Column::Queue => (p.text_muted, "circle"),
        Column::Working => (p.info, "loader-circle"),
        Column::Checks => (p.warning, "square"),
        Column::Ready => (p.success, "circle-play"),
        Column::Verified => (p.text_muted, "circle-check"),
    }
}

pub(super) fn tint(color: Hsla) -> Hsla {
    color.opacity(0.10)
}
