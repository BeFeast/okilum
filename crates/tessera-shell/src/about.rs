//! Shared product introduction and release information on every desktop platform.
use super::*;
use gpui_component::WindowExt;

pub(crate) const PROJECT_URL: &str = "https://git.oklabs.uk/BeFeast/tessera";
const TAGLINE: &str = "Your notes. Clearly connected.";
const DESCRIPTION: &str = "A fast Markdown reader and editor for your local, Obsidian-style vault. Built with native GPUI.";
const FEATURES: [&str; 4] = [
    "Read rich notes, tables, code and diagrams.",
    "Find the right passage with full-text search.",
    "Follow links, backlinks and note previews.",
    "Keep your Markdown files in your own folders.",
];

#[cfg(any(target_os = "macos", test))]
pub(crate) fn show_from_menu(cx: &mut App) {
    // Menu actions can run while the active window is already being updated.
    cx.defer(|cx| {
        if let Some(handle) = cx.active_window().or_else(|| cx.windows().first().copied()) {
            let _ = handle.update(cx, |_, window, cx| {
                window.activate_window();
                show_about(window, cx);
            });
        } else {
            let _ = cx.open_window(WindowOptions::default(), |window, cx| {
                let host = cx.new(|_| AboutHost);
                let root = cx.new(|cx| Root::new(host, window, cx));
                window.set_window_title("About Tessera");
                window.defer(cx, |window, cx| show_about_dialog(window, cx, true));
                root
            });
        }
    });
}

#[cfg(any(target_os = "macos", test))]
struct AboutHost;

#[cfg(any(target_os = "macos", test))]
impl Render for AboutHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .children(Root::render_dialog_layer(window, cx))
    }
}

pub(crate) fn show_about(window: &mut Window, cx: &mut App) {
    show_about_dialog(window, cx, false);
}

fn show_about_dialog(window: &mut Window, cx: &mut App, close_window: bool) {
    window.open_dialog(cx, move |dialog, _, cx| {
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
        dialog
            .title("About Tessera")
            .on_close(move |_, window, _| {
                if close_window {
                    window.remove_window();
                }
            })
            .width(px(520.))
            .child(
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
                                            .child("Tessera"),
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
                    .when(cfg!(windows), |view| {
                        view.child(
                            div()
                                .text_sm()
                                .text_color(palette.text_muted)
                                .child("This Windows build is read-only."),
                        )
                    })
                    .child(div().text_sm().child(format!(
                        "Version {} · Build {}",
                        env!("TESSERA_RELEASE_VERSION"),
                        env!("TESSERA_BUILD_VERSION")
                    )))
                    .child(
                        div()
                            .text_sm()
                            .text_color(palette.text_muted)
                            .child(format!("Update channel: {}", updater::channel())),
                    )
                    .child(links)
                    .when(updater::available(), |view| {
                        view.child(
                            Button::new("about-check-updates")
                                .label(updater::action_label())
                                .on_click(|_, _, cx| updater::activate(cx)),
                        )
                    })
                    .when(cfg!(target_os = "linux"), |view| {
                        view.child(
                            div()
                                .text_xs()
                                .text_color(palette.text_muted)
                                .child("Updates are managed by your system package manager."),
                        )
                    })
                    .when(cfg!(windows) && !updater::available(), |view| {
                        view.child(
                            div()
                                .text_xs()
                                .text_color(palette.text_muted)
                                .child("To update, download the latest Windows ZIP."),
                        )
                    }),
            )
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[gpui::test]
    fn menu_opens_shared_about_and_escape_dismisses(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| Reader::new(Opts::default(), window, cx));
            Root::new(reader, window, cx)
        });
        visual.run_until_parked();
        assert!(visual.debug_bounds("desktop-about").is_none());
        visual.update(|_, cx| show_from_menu(cx));
        visual.run_until_parked();
        assert!(visual.debug_bounds("desktop-about").is_some());
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        assert!(visual.debug_bounds("desktop-about").is_none());
    }
}
