//! Shared product introduction and release information on every desktop platform.
use super::*;

pub(crate) const PROJECT_URL: &str = "https://git.oklabs.uk/BeFeast/okilum";
const TAGLINE: &str = "Your notes. Clearly connected.";
const DESCRIPTION: &str = "A fast Markdown reader and editor for your local, Obsidian-style vault.";
const FEATURES: [&str; 4] = [
    "Read rich notes, tables, code and diagrams.",
    "Find the right passage with full-text search.",
    "Follow links, backlinks and note previews.",
    "Keep your Markdown files in your own folders.",
];

#[cfg(any(target_os = "macos", test))]
pub(crate) fn show_from_menu(cx: &mut App) {
    reader_settings::show_about(cx);
}

pub(crate) fn show_about(_window: &mut Window, cx: &mut App) {
    reader_settings::show_about(cx);
}

/// The update channel, named as one; never a bare «Beta» that could read as
/// the product stage (#995).
fn about_channel() -> String {
    match updater::channel() {
        channel @ ("Beta" | "Stable") => format!("{channel} channel"),
        // Linux: a package from an unidentified source says so below.
        _ if cfg!(target_os = "linux") => String::new(),
        other => other.into(),
    }
}

/// Where Linux updates come from (#1036): the pacman repository by name.
#[cfg(target_os = "linux")]
fn linux_update_source() -> &'static str {
    use updater::linux_repo::{current, Source};
    match current() {
        Source::Beta => "Updates come from the okilum-beta pacman repository.",
        Source::Stable => "Updates come from the okilum-stable pacman repository.",
        Source::Both => {
            "Both okilum-beta and okilum-stable are enabled in pacman.conf; enable only one."
        }
        Source::Unknown => "Updates come from your package manager.",
    }
}

pub(crate) fn content(cx: &mut App) -> impl IntoElement {
    let palette = brand::palette(cx);
    let mut links = h_flex().flex_wrap().gap_1();
    for (id, label, suffix) in [
        ("about-repo", "Repository", ""),
        ("about-releases", "Release notes", "/releases"),
        ("about-license", "MIT License", "/src/branch/main/LICENSE"),
        (
            "about-notices",
            "Third-party notices",
            "/src/branch/main/THIRD_PARTY_NOTICES.md",
        ),
    ] {
        let url = format!("{PROJECT_URL}{suffix}");
        links = links.child(
            Button::new(id)
                .ghost()
                .label(label)
                .on_click(move |_, _, cx| cx.open_url(&url)),
        );
    }
    v_flex()
        .debug_selector(|| "desktop-about".into())
        .gap_3()
        .text_color(palette.text)
        .child(
            h_flex()
                .gap_4()
                .items_center()
                .child(brand::logo(64., cx))
                .child(
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_xl()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Okilum"),
                        )
                        .child(TAGLINE),
                ),
        )
        .child(div().text_sm().child(DESCRIPTION))
        .child(
            v_flex()
                .gap_1()
                .text_sm()
                .children(FEATURES.map(|text| div().child(format!("• {text}")))),
        )
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .flex_1()
                        .text_sm()
                        .text_color(palette.text_muted)
                        .child(format!(
                            "Version {} · Build {}{}",
                            env!("OKILUM_RELEASE_VERSION"),
                            env!("OKILUM_BUILD_VERSION"),
                            if about_channel().is_empty() {
                                String::new()
                            } else {
                                format!(" · {}", about_channel())
                            }
                        )),
                )
                .when(updater::available(), |row| {
                    row.child(
                        reader_icon_button(
                            "about-check-updates",
                            IconName::RotateCw,
                            updater::action_label(),
                            cx,
                        )
                        .accessibility_label(updater::action_label())
                        .on_click(|_, _, cx| updater::activate(cx)),
                    )
                }),
        )
        .children(reader_settings::update_status_row("about", cx))
        .child(links)
        .when(cfg!(target_os = "linux"), |view| {
            view.child(
                div()
                    .text_size(px(12.))
                    .text_color(palette.text_muted)
                    .child({
                        #[cfg(target_os = "linux")]
                        {
                            linux_update_source()
                        }
                        #[cfg(not(target_os = "linux"))]
                        {
                            ""
                        }
                    }),
            )
        })
        .when(cfg!(windows) && !updater::available(), |view| {
            view.child(
                div()
                    .text_size(px(12.))
                    .text_color(palette.text_muted)
                    .child("To update, download the latest Windows ZIP."),
            )
        })
}
