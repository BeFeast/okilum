//! App actions shared by the Reader More menu on every platform.
#[cfg(all(unix, feature = "brain"))]
use crate::brain::app_quit::Quit;
#[cfg(not(all(unix, feature = "brain")))]
use crate::reader_app_menu::Quit;
use gpui::prelude::FluentBuilder;
use gpui_component::menu::{PopupMenu, PopupMenuItem};

#[cfg(all(not(windows), not(target_os = "macos")))]
const UPDATE_NOTICE: &str = "Updates come through pacman -Syu";
#[cfg(windows)]
const UPDATE_NOTICE: &str = "Windows updates are not available yet — download the new ZIP";

pub(super) fn append(menu: PopupMenu) -> PopupMenu {
    menu.separator()
        .item(
            PopupMenuItem::new("About Tessera").on_click(|_, window, cx| {
                show_about(window, cx);
            }),
        )
        .map(|menu| {
            #[cfg(windows)]
            let menu = if crate::updater::available() {
                menu.item(
                    PopupMenuItem::new(crate::updater::action_label())
                        .on_click(|_, _, cx| crate::updater::activate(cx)),
                )
            } else {
                menu.label(UPDATE_NOTICE)
            };
            #[cfg(all(not(windows), not(target_os = "macos")))]
            let menu = menu.label(UPDATE_NOTICE);
            menu
        })
        .menu("Quit Tessera", Box::new(Quit))
}

use crate::about::show_about;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;
    use ::core::prelude::v1::test;

    struct MoreMenuHarness {
        reader: Entity<Reader>,
        tooltip: Entity<gpui_base::TooltipOverlay>,
    }

    impl Render for MoreMenuHarness {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .child(reader_more_menu(
                    PathBuf::new(),
                    String::new(),
                    false,
                    self.reader.downgrade(),
                    cx,
                ))
                .child(self.tooltip.clone())
        }
    }

    #[gpui::test]
    fn more_button_is_visible_and_opens_and_dismisses_its_menu(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| Reader::new(Opts::default(), window, cx));
            let tooltip = cx.new(|_| gpui_base::TooltipOverlay::new());
            let menu = cx.new(|_| MoreMenuHarness { reader, tooltip });
            Root::new(menu, window, cx)
        });
        visual.run_until_parked();
        let menu_active = |visual: &mut VisualTestContext| {
            visual.update(|window, cx| {
                window.is_action_available(&gpui_base::actions::SelectDown, cx)
            })
        };
        assert!(!menu_active(visual), "Closed-state positive control");
        let trigger = visual
            .debug_bounds("reader-more")
            .expect("More button rendered");
        assert_eq!(trigger.size, size(px(28.), px(28.)));
        visual.simulate_click(trigger.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(menu_active(visual), "Click must focus the open menu");
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        assert!(!menu_active(visual), "Escape must dismiss the menu");
    }

    struct TooltipSentinel;

    impl Render for TooltipSentinel {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().debug_selector(|| "menu-tooltip".into()).size(px(20.))
        }
    }

    #[gpui::test]
    fn open_more_menu_suppresses_visible_and_delayed_tooltips(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let tooltip = cx.new(|_| gpui_base::TooltipOverlay::new());
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| Reader::new(Opts::default(), window, cx));
            let menu = cx.new(|_| MoreMenuHarness {
                reader,
                tooltip: tooltip.clone(),
            });
            Root::new(menu, window, cx)
        });
        visual.run_until_parked();
        let trigger = visual.debug_bounds("reader-more").unwrap();
        let request = |visual: &mut VisualTestContext| {
            visual.update(|window, cx| {
                tooltip.update(cx, |overlay, cx| {
                    overlay.request_show(
                        gpui_base::TooltipRequest::new(trigger, |_, cx| {
                            cx.new(|_| TooltipSentinel).into()
                        }),
                        window,
                        cx,
                    );
                })
            });
        };
        let settle = |visual: &mut VisualTestContext| {
            visual.run_until_parked();
            visual.executor().advance_clock(Duration::from_millis(600));
            visual.run_until_parked();
        };
        request(visual);
        settle(visual);
        assert!(
            visual.debug_bounds("menu-tooltip").is_some(),
            "Visible-tooltip positive control"
        );
        visual.simulate_click(trigger.center(), Modifiers::default());
        settle(visual);
        assert!(visual
            .update(|window, cx| window.is_action_available(&gpui_base::actions::SelectDown, cx)));
        assert!(
            visual.debug_bounds("menu-tooltip").is_none(),
            "Open menu removes existing tooltip"
        );
        request(visual);
        settle(visual);
        assert!(
            visual.debug_bounds("menu-tooltip").is_none(),
            "Open menu rejects new tooltip requests"
        );
        visual.simulate_keystrokes("escape");
        settle(visual);
        request(visual);
        visual.run_until_parked();
        visual.simulate_click(trigger.center(), Modifiers::default());
        settle(visual);
        assert!(
            visual.debug_bounds("menu-tooltip").is_none(),
            "Opening cancels pending hover delay"
        );
        visual.simulate_keystrokes("escape");
        settle(visual);
        request(visual);
        settle(visual);
        assert!(
            visual.debug_bounds("menu-tooltip").is_some(),
            "Tooltips recover after dismissal"
        );
    }

    #[gpui::test]
    fn about_dialog_opens_and_dismisses(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| Reader::new(Opts::default(), window, cx));
            Root::new(reader, window, cx)
        });
        visual.run_until_parked();
        visual.update(show_about);
        visual.run_until_parked();
        assert!(visual.debug_bounds("desktop-about").is_some());
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        assert!(visual.debug_bounds("desktop-about").is_none());
    }
}
