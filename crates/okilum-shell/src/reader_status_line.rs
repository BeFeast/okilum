//! One quiet bottom line, shared by file viewers.
use super::*;

/// Height of the bottom line; the toast stack stands above it (`reader_toast::init`).
pub(crate) const HEIGHT: Pixels = px(28.);

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Tone {
    #[default]
    Neutral,
    Warning,
}

#[derive(Clone, Default)]
pub(crate) struct StatusLine {
    pub icon: Option<Icon>,
    pub label: SharedString,
    pub text: SharedString,
    pub tone: Tone,
    pub actions: Vec<reader_viewer_chrome::ViewerAction>,
}

/// Empty descriptions mount nothing. A trailing position/choice control is
/// supplied separately so the read-only description never owns a GPUI element.
#[inline(never)]
pub(crate) fn render(
    status: &StatusLine,
    selector: &'static str,
    trailing: Option<AnyElement>,
    cx: &App,
) -> AnyElement {
    render_with_content(status, selector, None, trailing, cx)
}

#[inline(never)]
pub(crate) fn render_with_content(
    status: &StatusLine,
    selector: &'static str,
    content: Option<AnyElement>,
    trailing: Option<AnyElement>,
    cx: &App,
) -> AnyElement {
    if status.label.is_empty() && status.text.is_empty() && content.is_none() {
        return div().into_any_element();
    }
    let p = brand::palette(cx);
    let warning = status.tone == Tone::Warning;
    let full = if status.label.is_empty() {
        status.text.to_string()
    } else {
        format!("{} {}", status.label, status.text)
    };
    h_flex()
        .debug_selector(move || format!("{selector}-line"))
        .flex_none()
        .min_w_0()
        .h(HEIGHT)
        .px_3()
        .gap_2()
        .border_t_1()
        .border_color(p.border_subtle)
        .bg(if warning {
            brand::reader_palette(cx).warning_tint
        } else {
            p.surface
        })
        .text_size(px(12.))
        .line_height(px(16.))
        .text_color(if warning { p.text } else { p.text_muted })
        .children(status.icon.clone().map(|icon| {
            icon.size(px(14.))
                .text_color(if warning { p.warning } else { p.text_muted })
        }))
        .child(content.unwrap_or_else(|| {
            h_flex()
                .id(selector)
                .debug_selector(move || selector.into())
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .gap_1()
                .when(!status.label.is_empty(), |row| {
                    row.child(
                        div()
                            .flex_none()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.text)
                            .child(status.label.clone()),
                    )
                })
                .child(div().truncate().child(status.text.clone()))
                .tooltip(move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(full.clone()).build(window, cx)
                })
                .into_any_element()
        }))
        .children(
            status
                .actions
                .iter()
                .take(2)
                .map(|action| action.button(false)),
        )
        .children(trailing)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    struct Gallery;
    impl Render for Gallery {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            v_flex()
                .size_full()
                .child(render(
                    &StatusLine {
                        icon: Some(Icon::new(IconName::Info)),
                        label: "Large file.".into(),
                        text: "3.4 MB on a single line.".into(),
                        ..Default::default()
                    },
                    "status-neutral",
                    None,
                    cx,
                ))
                .child(render(
                    &StatusLine {
                        icon: Some(Icon::new(IconName::TriangleAlert)),
                        label: "Not valid JSON.".into(),
                        text: "Line 12, column 3.".into(),
                        tone: Tone::Warning,
                        ..Default::default()
                    },
                    "status-warning",
                    None,
                    cx,
                ))
                .child(render(&StatusLine::default(), "status-empty", None, cx))
        }
    }

    #[gpui::test]
    fn neutral_and_warning_status_gallery_uses_28_pixel_lines_in_light_dark_and_high_contrast(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::test_init);
        let (_, visual) =
            cx.add_window_view(|window, cx| Root::new(cx.new(|_| Gallery), window, cx));
        for theme in [brand::ThemeId::Okilum, brand::ThemeId::HighContrast] {
            for mode in [
                gpui_component::ThemeMode::Light,
                gpui_component::ThemeMode::Dark,
            ] {
                visual.update(|_, cx| {
                    cx.set_global(brand::ThemeChoice(theme));
                    gpui_component::Theme::change(mode, None, cx);
                    brand::apply_theme(cx);
                });
                visual.run_until_parked();
                for id in ["status-neutral-line", "status-warning-line"] {
                    assert_eq!(visual.debug_bounds(id).unwrap().size.height, px(28.));
                }
                assert!(visual.debug_bounds("status-empty").is_none());
            }
        }
    }
}
