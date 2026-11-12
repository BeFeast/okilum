//! Shared 28 px view switch: full labels, active label with icons, or one menu.
use super::*;
use gpui_component::menu::DropdownMenu as _;
use reader_viewer_chrome::{Disposition, LayoutItem, ViewerView};

#[derive(Clone, Copy)]
pub(crate) enum Form {
    Full,
    Icons,
    Menu,
}

pub(crate) fn layout(views: &[ViewerView], window: &Window) -> Option<LayoutItem> {
    if views.len() < 2 {
        return None;
    }
    let label_width = |label: &str| toolbar_text_width(label, FontWeight::SEMIBOLD, window);
    let full = 4.
        + views
            .iter()
            .map(|view| 44. + label_width(&view.action.label))
            .sum::<f32>();
    let icons = 4.
        + 28. * views.len() as f32
        + views
            .iter()
            .filter(|view| view.active)
            .map(|view| 16. + label_width(&view.action.label))
            .sum::<f32>();
    let mut item = LayoutItem::pinned("views", full, icons);
    item.steps = vec![reader_viewer_chrome::overflow::OverflowStep {
        priority: 25,
        width: icons,
        disposition: Disposition::Collapsed,
    }];
    Some(item)
}

#[inline(never)]
pub(crate) fn render(views: &[ViewerView], form: Form, cx: &App) -> AnyElement {
    let tokens = brand::palette(cx);
    if views.len() < 2 {
        return div().into_any_element();
    }
    if matches!(form, Form::Menu) {
        let active = views.iter().find(|view| view.active).unwrap_or(&views[0]);
        let items = views.to_vec();
        return Button::new("code-view-menu")
            .ghost()
            .h(px(28.))
            .px_2()
            .label(active.action.label.clone())
            .dropdown_caret(true)
            .debug_selector(|| "code-view-menu".into())
            .accessibility_label("View")
            .dropdown_menu_with_anchor(Anchor::TopLeft, move |mut menu, _, _| {
                for view in &items {
                    let mut action = view.action.clone();
                    action.toggled = view.active;
                    menu = menu.item(action.menu_item());
                }
                menu
            })
            .into_any_element();
    }
    h_flex()
        .debug_selector(|| "reader-segmented".into())
        .flex_none()
        .p(px(2.))
        .rounded(px(6.))
        .bg(tokens.border_subtle.opacity(0.5))
        .children(views.iter().map(|view| {
            let labelled = matches!(form, Form::Full) || view.active;
            let action = &view.action;
            let invoke = action.invoke.clone();
            Button::new(action.id)
                .ghost()
                .h(px(28.))
                .min_w(px(28.))
                .px_1()
                .rounded(px(4.))
                .icon(action.icon.clone().size(px(14.)))
                .when(labelled, |b| b.label(action.label.clone()))
                .accessibility_label(action.label.clone())
                .selected(view.active)
                .disabled(!action.enabled)
                .text_size(px(11.))
                .text_color(if view.active {
                    tokens.text
                } else {
                    tokens.text_muted
                })
                .when(view.active, |b| {
                    b.bg(tokens.surface_raised)
                        .border_1()
                        .border_color(tokens.border_subtle)
                        .font_weight(FontWeight::SEMIBOLD)
                })
                .tooltip(
                    action
                        .disabled_reason
                        .clone()
                        .unwrap_or_else(|| action.tooltip.clone()),
                )
                .debug_selector({
                    let id = action.id;
                    move || id.into()
                })
                .on_click(move |_, window, cx| invoke(window, cx))
        }))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    struct Gallery {
        views: Vec<ViewerView>,
        form: Form,
    }
    impl Render for Gallery {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            v_flex()
                .size_full()
                .child(render(&self.views, self.form, cx))
        }
    }
    #[gpui::test]
    fn code2809_segmented_full_icons_disabled_and_menu_are_actionable_in_all_themes(
        cx: &mut TestAppContext,
    ) {
        use reader_viewer_chrome::ViewerAction;
        cx.update(crate::test_init);
        let clicked = std::rc::Rc::new(std::cell::Cell::new(None));
        let views = ["Table", "Tree", "Formatted", "Raw"]
            .into_iter()
            .enumerate()
            .map(|(i, label)| {
                let clicked = clicked.clone();
                let mut action = ViewerAction::new(
                    [
                        "segment-table",
                        "segment-tree",
                        "segment-formatted",
                        "segment-raw",
                    ][i],
                    Icon::new(IconName::Search),
                    label,
                    std::rc::Rc::new(move |_, _| clicked.set(Some(i))),
                );
                if i == 2 {
                    action.enabled = false;
                    action.disabled_reason =
                        Some("Not available: the file is not valid JSON".into());
                }
                ViewerView {
                    action,
                    active: i == 1,
                }
            })
            .collect::<Vec<_>>();
        let mut gallery = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|_| Gallery {
                views,
                form: Form::Full,
            });
            gallery = Some(view.clone());
            Root::new(view, window, cx)
        });
        let gallery = gallery.unwrap();
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
                for form in [Form::Full, Form::Icons] {
                    gallery.update(visual, |gallery, cx| {
                        gallery.form = form;
                        cx.notify();
                    });
                    visual.run_until_parked();
                    visual.update(|window, cx| {
                        window.refresh();
                        window.draw(cx).clear(cx);
                    });
                    for id in [
                        "segment-table",
                        "segment-tree",
                        "segment-formatted",
                        "segment-raw",
                    ] {
                        assert_eq!(visual.debug_bounds(id).unwrap().size.height, px(28.));
                    }
                    let disabled = visual.debug_bounds("segment-formatted").unwrap();
                    clicked.set(None);
                    visual.simulate_click(disabled.center(), Modifiers::default());
                    visual.run_until_parked();
                    assert_eq!(clicked.get(), None);
                    visual.update(|window, cx| {
                        window.refresh();
                        window.draw(cx).clear(cx);
                    });
                    let raw = visual.debug_bounds("segment-raw").unwrap();
                    visual.simulate_click(raw.center(), Modifiers::default());
                    visual.run_until_parked();
                    assert_eq!(clicked.get(), Some(3));
                    if matches!(form, Form::Icons) {
                        visual.update(|window, cx| {
                            window.refresh();
                            window.draw(cx).clear(cx);
                        });
                        assert!(
                            visual.debug_bounds("segment-tree").unwrap().size.width
                                > visual.debug_bounds("segment-table").unwrap().size.width,
                            "active label stays visible beside icons"
                        );
                    }
                }
            }
        }
        gallery.update(visual, |gallery, cx| {
            gallery.form = Form::Menu;
            cx.notify();
        });
        visual.run_until_parked();
        visual.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        let menu = visual.debug_bounds("code-view-menu").unwrap();
        assert_eq!(menu.size.height, px(28.));
        clicked.set(None);
        visual.simulate_click(menu.center(), Modifiers::default());
        visual.run_until_parked();
        visual.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            assert!(
                !gpui_base::GlobalState::native_overlays(window, cx).is_empty(),
                "view menu mounted"
            );
        });
        visual.simulate_keystrokes("down down down enter");
        visual.run_until_parked();
        assert_eq!(
            clicked.get(),
            Some(3),
            "menu skips disabled Formatted and invokes Raw"
        );
    }
}
