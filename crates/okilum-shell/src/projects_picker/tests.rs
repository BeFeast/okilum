use super::*;
use gpui::{point, TestAppContext};
use gpui_component::Root;
use okilum_t3::projects::{Kind, Manager};
use std::{cell::RefCell, rc::Rc};

struct Host(Entity<ProjectsPicker>);
impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .child(mount(self.0.clone(), point(px(24.), px(48.))))
    }
}
fn catalog() -> PickerModel {
    let vault = PathBuf::from("vault");
    let projects = [
        ("Dev/Areas/Editor", "Editor", Kind::Code),
        ("Music/Projects/Album", "Album", Kind::Hybrid),
        ("Music/Areas/Studio", "Studio", Kind::Knowledge),
    ]
    .map(|(folder, name, kind)| Project {
        folder: vault.join(folder),
        name: name.into(),
        kind,
        environment: None,
        manager_project: None,
        manager: Manager::NotStarted,
    })
    .to_vec();
    PickerModel::new(
        vault,
        Discovery {
            projects,
            errors: Vec::new(),
        },
        Vec::new(),
    )
}

#[gpui::test]
fn popover_search_keyboard_and_activation_use_the_same_model(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.set_reduce_motion(true);
        gpui_component::init(cx);
    });
    let selected = Rc::new(RefCell::new(Vec::new()));
    let dismissed = Rc::new(RefCell::new(0));
    let mut entity = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let picker = cx.new(|cx| ProjectsPicker::new(catalog(), Presentation::Popover, window, cx));
        let selected = selected.clone();
        cx.subscribe(&picker, move |_, _, event: &Selected, _| {
            selected.borrow_mut().push(event.0.folder.clone())
        })
        .detach();
        let dismissed = dismissed.clone();
        cx.subscribe(&picker, move |_, _, _: &DismissEvent, _| {
            *dismissed.borrow_mut() += 1
        })
        .detach();
        entity = Some(picker.clone());
        let host = cx.new(|_| Host(picker));
        Root::new(host, window, cx)
    });
    let picker = entity.unwrap();
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let bounds = visual
        .debug_bounds("projects-picker")
        .expect("positive control: picker rendered");
    assert_eq!(
        bounds.origin.x,
        px(24.),
        "popover stays left aligned below the switcher"
    );
    visual.simulate_input("music");
    visual.run_until_parked();
    picker.read_with(visual, |p, _| {
        assert_eq!(p.model.query(), "music");
        assert_eq!(
            p.items
                .iter()
                .filter(|i| matches!(i, Item::Project(_)))
                .count(),
            2
        );
    });
    visual.simulate_keystrokes("down");
    visual.simulate_keystrokes("enter");
    assert_eq!(
        selected.borrow().as_slice(),
        [PathBuf::from("vault/Music/Areas/Studio")]
    );
    picker.read_with(visual, |p, _| {
        assert!(
            p.model.recent().is_empty(),
            "a request must not claim successful activation"
        )
    });
    picker.update_in(visual, |p, _, cx| {
        p.record_opened(Path::new("vault/Music/Areas/Studio"), cx)
    });
    picker.read_with(visual, |p, _| {
        assert_eq!(p.model.groups()[0].label, "Recent")
    });
    visual.simulate_keystrokes("escape");
    picker.read_with(visual, |p, _| assert!(p.model.query().is_empty()));
    assert_eq!(*dismissed.borrow(), 0);
    visual.simulate_keystrokes("escape");
    assert_eq!(*dismissed.borrow(), 1);
}

#[gpui::test]
fn embedded_palette_updates_filter_and_discovery_without_stale_selection(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.set_reduce_motion(true);
        gpui_component::init(cx);
    });
    let mut entity = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let picker =
            cx.new(|cx| ProjectsPicker::new(catalog(), Presentation::Embedded, window, cx));
        entity = Some(picker.clone());
        Root::new(picker, window, cx)
    });
    let picker = entity.unwrap();
    picker.update_in(visual, |p, window, cx| {
        p.set_query("music", window, cx);
        p.set_filter(KindFilter::Knowledge, cx);
        assert_eq!(
            p.selected.as_ref().unwrap().folder,
            Path::new("vault/Music/Areas/Studio")
        );
        p.update_discovery(Discovery::default(), cx);
        assert!(p.selected.is_none());
        assert!(
            !p.confirm(cx),
            "removed projects cannot be activated via a stale selection"
        );
    });
}
