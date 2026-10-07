//! Theme picker (#349): compact swatch cards, one per `brand::ThemeId`.
//!
//! Hosted in Settings → Appearance after its redesign (#623).
//! Selecting a card applies the theme live and stores it app-wide; the
//! light/dark/system mode is left untouched.
use super::*;

const CARD_WIDTH: f32 = 120.;
const PREVIEW_HEIGHT: f32 = 64.;

pub(crate) struct ThemePicker {
    /// Never write presentation state into this vault (see `save_appearance`).
    vault: Option<PathBuf>,
    /// One keyboard stop per card, in `ThemeId::ALL` order.
    focus: Vec<FocusHandle>,
}

impl ThemePicker {
    pub(crate) fn new(vault: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        Self {
            vault,
            focus: brand::ThemeId::ALL
                .map(|_| cx.focus_handle().tab_stop(true))
                .to_vec(),
        }
    }
}

impl Render for ThemePicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex().w_full().flex_wrap().gap_3().children(
            brand::ThemeId::ALL
                .into_iter()
                .zip(self.focus.iter())
                .map(|(theme, focus)| card(theme, focus, self.vault.clone(), window, cx)),
        )
    }
}

/// The theme's own sidebar, surface, text, muted, link and accent in the
/// variant the window currently shows. Selection is a ring plus a check glyph,
/// both always laid out so selecting never shifts the row.
fn card(
    theme: brand::ThemeId,
    focus: &FocusHandle,
    vault: Option<PathBuf>,
    window: &Window,
    cx: &App,
) -> impl IntoElement {
    let live = brand::palette(cx);
    let selected = brand::theme_id(cx) == theme;
    let ringed = selected || focus.is_focused(window);
    let t = brand::theme_palette(theme, Theme::global(cx).is_dark());
    let id = format!("theme-picker-{}", theme.key());
    let bar = |width: f32, height: f32, color: Hsla| {
        div()
            .w(px(width))
            .h(px(height))
            .rounded(px(height / 2.))
            .bg(color)
    };
    let key_vault = vault.clone();
    v_flex()
        .id(SharedString::from(id.clone()))
        .debug_selector(move || id.clone())
        .group("theme-card")
        .w(px(CARD_WIDTH))
        .gap(px(6.))
        .cursor_pointer()
        .track_focus(focus)
        .on_click(move |_, window, cx| set_theme(theme, vault.as_deref(), window, cx))
        .on_key_down(move |event, window, cx| {
            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                set_theme(theme, key_vault.as_deref(), window, cx);
                cx.stop_propagation();
            }
        })
        .child(
            div()
                .p(px(2.))
                .rounded(px(12.))
                .border_2()
                .border_color(if ringed {
                    live.focus
                } else {
                    gpui::transparent_black()
                })
                .when(!ringed, |ring| {
                    ring.group_hover("theme-card", |s| s.border_color(live.border))
                })
                .child(
                    h_flex()
                        .h(px(PREVIEW_HEIGHT))
                        .w_full()
                        .rounded(px(8.))
                        .overflow_hidden()
                        .border_1()
                        .border_color(t.border_subtle)
                        .child(div().w(px(22.)).h_full().flex_none().bg(t.sidebar))
                        .child(
                            v_flex()
                                .flex_1()
                                .h_full()
                                .p(px(8.))
                                .gap(px(5.))
                                .bg(t.surface)
                                .child(bar(48., 6., t.text))
                                .child(bar(60., 4., t.text_muted))
                                .child(bar(36., 4., t.link))
                                .child(div().mt_auto().child(bar(22., 8., t.accent))),
                        ),
                ),
        )
        .child(
            h_flex()
                .gap_1()
                .items_center()
                .text_sm()
                .text_color(if selected { live.text } else { live.text_muted })
                .child(
                    Icon::new(IconName::Check)
                        .size(px(14.))
                        .text_color(live.accent)
                        .opacity(if selected { 1. } else { 0. }),
                )
                .child(theme.name()),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui_component::ThemeMode;

    #[gpui::test]
    fn cards_select_one_app_wide_theme_in_place(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(AppearancePreference(Some(ThemeMode::Dark)));
        });
        let (_, visual) = cx.add_window_view(|window, cx| {
            // As at startup: the themed typography is in place before layout.
            sync_appearance(window, cx);
            let picker = cx.new(|cx| ThemePicker::new(None, cx));
            Root::new(picker, window, cx)
        });
        visual.run_until_parked();
        visual.update(|_, cx| assert_eq!(brand::theme_id(cx), brand::ThemeId::Tessera));
        let card = |visual: &mut VisualTestContext, theme: brand::ThemeId| {
            visual
                .debug_bounds(format!("theme-picker-{}", theme.key()).leak())
                .expect("theme card rendered")
        };
        // Every theme is offered, at one size, and selection never moves a card.
        let before: Vec<_> = brand::ThemeId::ALL
            .into_iter()
            .map(|theme| card(visual, theme))
            .collect();
        assert!(before.iter().all(|b| b.size == before[0].size));
        for theme in [brand::ThemeId::Nord, brand::ThemeId::Paper] {
            let bounds = card(visual, theme);
            visual.simulate_click(bounds.center(), Modifiers::default());
            visual.run_until_parked();
            visual.update(|_, cx| {
                assert_eq!(brand::theme_id(cx), theme);
                // The light/dark choice is independent and kept.
                assert_eq!(appearance_label(cx), "Dark");
                assert_eq!(
                    brand::palette(cx).surface,
                    brand::theme_palette(theme, true).surface
                );
            });
            let after: Vec<_> = brand::ThemeId::ALL
                .into_iter()
                .map(|theme| card(visual, theme))
                .collect();
            assert_eq!(before, after);
        }
        visual.update(|window, cx| set_theme(brand::ThemeId::Tessera, None, window, cx));
    }

    #[test]
    fn appearance_settings_round_trip_theme_and_accept_older_files() {
        for theme in brand::ThemeId::ALL {
            let (mode, choice) =
                parse_appearance_settings(&appearance_settings_json("dark", theme));
            assert!(mode.0 == Some(ThemeMode::Dark));
            assert_eq!(choice.0, theme);
        }
        // Written before #349: no theme key keeps the default theme.
        let (mode, choice) =
            parse_appearance_settings(&serde_json::json!({ "appearance": "light" }));
        assert!(mode.0 == Some(ThemeMode::Light));
        assert_eq!(choice.0, brand::ThemeId::Tessera);
        // An unknown theme from a newer build is not guessed at.
        let (mode, choice) = parse_appearance_settings(
            &serde_json::json!({ "appearance": "system", "theme": "solarized" }),
        );
        assert!(mode.0.is_none());
        assert_eq!(choice.0, brand::ThemeId::Tessera);
    }
}
