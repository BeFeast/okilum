//! The same Find controls in a note's floating bar and a file's header slot.
use super::*;
use std::rc::Rc;

pub(crate) mod editor;
#[cfg(test)]
mod tests;

pub(crate) type Invoke = reader_viewer_chrome::Invoke;
pub(crate) type Step = Rc<dyn Fn(i32, &mut Window, &mut App)>;

/// The viewer owns query/filter semantics. This component owns only presentation.
#[derive(Clone)]
pub(crate) struct Target {
    pub input: Entity<InputState>,
    pub focus_override: Option<Invoke>,
    pub scope: SharedString,
    pub count: Option<SharedString>,
    pub match_case: Option<(bool, Invoke)>,
    pub step: Option<Step>,
    pub steps_visible: bool,
    pub close: Invoke,
    pub large_file: bool,
}

impl Target {
    pub(crate) fn set_query(&self, query: &str, window: &mut Window, cx: &mut App) {
        self.input.update(cx, |input, cx| {
            input.set_value(query, window, cx);
            cx.emit(gpui_component::input::InputEvent::Change);
        });
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &mut App) {
        if let Some(focus) = &self.focus_override {
            focus(window, cx);
        } else {
            self.focus_field(window, cx);
        }
    }

    /// Clicking this viewer's Find icon always opens its own field. The
    /// shortcut may instead follow a focused nested pane via focus_override.
    pub(crate) fn focus_field(&self, window: &mut Window, cx: &mut App) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Presentation {
    Floating,
    Header {
        width: f32,
        focused: bool,
        max_width: f32,
    },
}

/// Header expansion is an absolutely positioned child of a fixed-width slot.
/// Only its left edge changes; controls to the right and the document stay put.
#[inline(never)]
pub(crate) fn render(target: &Target, presentation: Presentation, cx: &App) -> AnyElement {
    let floating = matches!(presentation, Presentation::Floating);
    let palette = brand::palette(cx);
    let focused = matches!(presentation, Presentation::Header { focused: true, .. });
    let empty = target.input.read(cx).value().is_empty();
    let mut row = h_flex()
        .id(if floating {
            "find-bar"
        } else {
            "viewer-find-field"
        })
        .debug_selector(move || {
            if floating {
                "find-bar"
            } else {
                "viewer-find-field"
            }
            .into()
        })
        .gap_1()
        .pl_2()
        .pr_1()
        .bg(if floating {
            cx.theme().popover
        } else {
            palette.canvas
        })
        .border_1()
        .border_color(if focused {
            palette.focus
        } else {
            cx.theme().border
        })
        .rounded(px(if floating { 8. } else { 6. }));
    row = if floating {
        row.absolute()
            .top(px(10.))
            .right(px(18.))
            .py_1()
            .shadow_md()
    } else {
        row.occlude()
            .absolute()
            .right_0()
            .h(px(28.))
            .w_full()
            .key_context("ViewerFind")
    };
    let input = Input::new(&target.input)
        .appearance(false)
        .when(!floating, |input| input.small())
        .cleanable(floating)
        .prefix(
            Icon::new(IconName::Search)
                .small()
                .text_color(cx.theme().muted_foreground),
        );
    row = row.child(
        div()
            .debug_selector(|| "viewer-find-query".into())
            .when(floating, |d| d.w(px(220.)))
            .when(!floating, |d| d.flex_1().min_w_0())
            .child(input),
    );
    if let Some(count) = target.count.as_ref().filter(|count| !count.is_empty()) {
        row = row.child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .when(floating, |d| d.min_w(px(56.)))
                .flex_none()
                .child(count.clone()),
        );
    }
    if let Some((sensitive, invoke)) = target.match_case.as_ref().filter(|_| floating || focused) {
        let invoke = invoke.clone();
        row = row.child(
            control("find-case-sensitive", IconName::CaseSensitive, floating)
                .selected(*sensitive)
                .tooltip("Match case")
                .on_click(move |_, window, cx| invoke(window, cx)),
        );
    }
    if let Some(step) = target.step.as_ref().filter(|_| floating || focused) {
        for (id, icon, direction) in [
            ("find-prev", IconName::ChevronUp, -1),
            ("find-next", IconName::ChevronDown, 1),
        ] {
            let step = step.clone();
            row = row.child(
                control(id, icon, floating)
                    .when(!target.steps_visible, |b| b.invisible())
                    .tooltip(reader_shortcuts::hint(
                        if direction < 0 {
                            "Previous match"
                        } else {
                            "Next match"
                        },
                        &gpui_component::input::Enter {
                            secondary: false,
                            shift: direction < 0,
                        },
                        cx,
                    ))
                    .on_click(move |_, window, cx| step(direction, window, cx)),
            );
        }
    }
    let close = target.clone();
    let dismiss = target.clone();
    if floating || focused || !empty {
        row = row.child(
            control("find-close", IconName::Close, floating)
                .tooltip(reader_shortcuts::hint("Close", &Dismiss, cx))
                .on_click(move |_, window, cx| {
                    if !floating {
                        close.set_query("", window, cx);
                    }
                    (close.close)(window, cx);
                }),
        );
    }
    if !floating {
        row = row.on_action(move |_: &Dismiss, window, cx| {
            dismiss.set_query("", window, cx);
            (dismiss.close)(window, cx);
        });
    }
    match presentation {
        Presentation::Floating => row.into_any_element(),
        Presentation::Header {
            width,
            focused,
            max_width,
        } => {
            let width = if target.large_file {
                width.max(128.)
            } else {
                width
            };
            let expanded = if focused {
                width.max(272.).min(max_width)
            } else {
                width
            };
            if width < 128. && !focused && !target.large_file {
                let target = target.clone();
                return reader_icon_button(
                    "viewer-find",
                    IconName::Search,
                    reader_shortcuts::hint(&target.scope, &FindInNote, cx),
                    cx,
                )
                .debug_selector(|| "viewer-find".into())
                .on_click(move |_, window, cx| target.focus_field(window, cx))
                .into_any_element();
            }
            let mut field = row.w(px(expanded));
            if empty && !focused && width >= 200. {
                let hint = reader_shortcuts::shortcut(&FindInNote, cx).unwrap_or_default();
                field = field.child(reader_shortcuts::key_cap(
                    hint,
                    palette,
                    brand::reader_palette(cx),
                ));
            }
            div()
                .relative()
                .flex_none()
                .w(px(width))
                .h(px(28.))
                .debug_selector(|| "viewer-find-slot".into())
                .child(field)
                .into_any_element()
        }
    }
}

fn control(id: &'static str, icon: IconName, floating: bool) -> Button {
    Button::new(id)
        .ghost()
        .small()
        .icon(icon)
        .when(!floating, |b| b.w(px(28.)).h(px(28.)).p_0())
        .debug_selector(move || id.into())
}
