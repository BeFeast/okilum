use super::*;
use gpui::{Modifiers, TestAppContext, VisualTestContext};
use gpui_component::Root;
use okilum_t3::projects::icon::{monogram_color, DEFAULT_LUCIDE};
use std::{cell::RefCell, rc::Rc};

struct Host(Entity<ProjectIconDialog>);
impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().p_4().child(self.0.clone())
    }
}

fn open(
    cx: &mut TestAppContext,
    current: Option<ProjectIcon>,
) -> (
    Entity<ProjectIconDialog>,
    &mut VisualTestContext,
    Rc<RefCell<Vec<IconDialogEvent>>>,
) {
    cx.update(|cx| {
        cx.set_reduce_motion(true);
        gpui_component::init(cx);
        crate::project_avatar::set_lucide_names(
            ["bot", "folder", "rocket", "book-open"].map(str::to_owned),
            cx,
        );
    });
    let events = Rc::new(RefCell::new(Vec::new()));
    let mut entity = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let dialog = cx.new(|cx| ProjectIconDialog::new("ai-bills", current.as_ref(), window, cx));
        let seen = events.clone();
        cx.subscribe(&dialog, move |_, _, event: &IconDialogEvent, _| {
            seen.borrow_mut().push(event.clone())
        })
        .detach();
        entity = Some(dialog.clone());
        let host = cx.new(|_| Host(dialog));
        Root::new(host, window, cx)
    });
    visual.run_until_parked();
    (entity.unwrap(), visual, events)
}

fn click(visual: &mut VisualTestContext, selector: &'static str) {
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is drawn"));
    visual.simulate_click(bounds.center(), Modifiers::none());
    visual.run_until_parked();
}

/// T3's dialog: tabs, the eighteen swatches with one ringed, the icon grid
/// popular-first, Cancel / Save icon, and Reset only with a manual icon.
#[gpui::test]
fn the_dialog_saves_the_tab_that_is_shown(cx: &mut TestAppContext) {
    let (dialog, visual, events) = open(cx, None);
    visual.update(|window, cx| window.draw(cx).clear(cx));
    for selector in [
        "icon-tabs",
        "icon-colors",
        "icon-grid",
        "icon-save",
        "icon-cancel",
    ] {
        assert!(visual.debug_bounds(selector).is_some(), "{selector}");
    }
    assert!(visual.debug_bounds("icon-color-17").is_some(), "18 colours");
    assert!(visual.debug_bounds("icon-color-18").is_none());
    let names = dialog.read_with(visual, |d, cx| d.visible_icons(cx));
    assert_eq!(names, ["bot", "book-open", "folder", "rocket"]);
    dialog.read_with(visual, |d, _| {
        assert_eq!(d.draft().tab, PickerTab::Icons);
        assert_eq!(d.draft().lucide, DEFAULT_LUCIDE);
        assert_eq!(d.draft().color, monogram_color("ai-bills"));
    });

    // Reset needs a manual icon; with none it does nothing.
    click(visual, "icon-reset");
    assert!(events.borrow().is_empty());

    click(visual, "icon-color-3");
    click(visual, "icon-choice-3");
    click(visual, "icon-save");
    assert_eq!(
        events.borrow().last(),
        Some(&IconDialogEvent::Save(ProjectIcon::Lucide {
            name: "rocket".into(),
            color: IconColor::Amber
        }))
    );

    click(visual, "icon-tab-1");
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        visual.debug_bounds("icon-colors").is_none(),
        "emoji has no colour"
    );
    click(visual, "emoji-choice-2");
    click(visual, "icon-save");
    assert_eq!(
        events.borrow().last(),
        Some(&IconDialogEvent::Save(ProjectIcon::Emoji {
            emoji: "🚀".into()
        }))
    );

    click(visual, "icon-tab-2");
    click(visual, "icon-color-11");
    click(visual, "icon-save");
    assert_eq!(
        events.borrow().last(),
        Some(&IconDialogEvent::Save(ProjectIcon::Monogram {
            text: "AB".into(),
            color: IconColor::Blue
        }))
    );
    click(visual, "icon-cancel");
    assert_eq!(events.borrow().last(), Some(&IconDialogEvent::Cancel));
}

#[gpui::test]
fn the_dialog_opens_on_the_manual_icon_and_offers_reset(cx: &mut TestAppContext) {
    let current = ProjectIcon::Monogram {
        text: "Ok".into(),
        color: IconColor::Teal,
    };
    let (dialog, visual, events) = open(cx, Some(current.clone()));
    dialog.read_with(visual, |d, _| {
        assert_eq!(d.draft().tab, PickerTab::Monogram);
        assert_eq!(d.draft().result(), Some(current));
    });
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("icon-monogram-input").is_some());
    click(visual, "icon-reset");
    assert_eq!(events.borrow().last(), Some(&IconDialogEvent::Reset));
}

/// A window that draws Root's dialog layer, as the Reader does.
struct DialogHost;
impl Render for DialogHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .children(Root::render_dialog_layer(window, cx))
    }
}

/// #1627: Save in the real dialog reaches the host's handler with the
/// window it was pressed in, for every kind, Reset and Cancel; the host
/// closes the dialog from there.
#[gpui::test]
fn the_real_dialog_hands_each_choice_to_the_host_in_its_window(cx: &mut TestAppContext) {
    use gpui_component::WindowExt as _;
    cx.update(|cx| {
        cx.set_reduce_motion(true);
        gpui_component::init(cx);
        crate::project_avatar::set_lucide_names(
            ["bot", "folder", "rocket", "book-open"].map(str::to_owned),
            cx,
        );
    });
    let (_, visual) = cx.add_window_view(|window, cx| {
        let host = cx.new(|_| DialogHost);
        Root::new(host, window, cx)
    });
    let choices: Rc<RefCell<Vec<(IconDialogEvent, gpui::AnyWindowHandle)>>> = Default::default();
    let open_dialog = |visual: &mut VisualTestContext, current: Option<ProjectIcon>| {
        let seen = choices.clone();
        visual.update(|window, cx| {
            let dialog =
                cx.new(|cx| ProjectIconDialog::new("ai-bills", current.as_ref(), window, cx));
            dialog.update(cx, |dialog, _| {
                dialog.on_choice(move |event, window, cx| {
                    seen.borrow_mut()
                        .push((event.clone(), window.window_handle()));
                    window.close_dialog(cx);
                })
            });
            window.open_dialog(cx, move |d, _, _| {
                d.title(TITLE).width(gpui::px(520.)).child(dialog.clone())
            });
        });
        visual.run_until_parked();
    };
    let this_window = visual.update(|window, _| window.window_handle());

    open_dialog(visual, None);
    click(visual, "icon-color-6");
    click(visual, "icon-choice-3");
    click(visual, "icon-save");
    open_dialog(visual, None);
    click(visual, "icon-tab-1");
    click(visual, "emoji-choice-2");
    click(visual, "icon-save");
    open_dialog(visual, None);
    click(visual, "icon-tab-2");
    click(visual, "icon-save");
    open_dialog(
        visual,
        Some(ProjectIcon::Emoji {
            emoji: "🧠".into()
        }),
    );
    click(visual, "icon-reset");
    open_dialog(visual, None);
    click(visual, "icon-cancel");

    let seen = choices.borrow();
    let events: Vec<_> = seen.iter().map(|(event, _)| event.clone()).collect();
    assert_eq!(
        events,
        [
            IconDialogEvent::Save(ProjectIcon::Lucide {
                name: "rocket".into(),
                color: IconColor::Green,
            }),
            IconDialogEvent::Save(ProjectIcon::Emoji {
                emoji: "🚀".into()
            }),
            IconDialogEvent::Save(ProjectIcon::Monogram {
                text: "AB".into(),
                color: monogram_color("ai-bills"),
            }),
            IconDialogEvent::Reset,
            IconDialogEvent::Cancel,
        ]
    );
    assert!(seen.iter().all(|(_, window)| *window == this_window));
    visual.update(|window, cx| assert!(!window.has_active_dialog(cx), "the host closed it"));
}
