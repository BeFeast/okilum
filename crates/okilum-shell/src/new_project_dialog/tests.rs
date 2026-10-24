use super::*;
use gpui::{Modifiers, TestAppContext, VisualTestContext};
use gpui_component::Root;
use okilum_t3::projects::icon::IconColor;
use std::{cell::RefCell, rc::Rc};

struct Host(Entity<NewProjectDialog>);
impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().p_4().child(self.0.clone())
    }
}

/// A temporary vault with `HomeLab/Mesh` and the project `Dev/Areas/app`.
struct Vault(PathBuf);
impl Vault {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("okilum-new-project-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("HomeLab/Mesh")).unwrap();
        std::fs::create_dir_all(root.join("Dev/Areas/app/ops")).unwrap();
        std::fs::write(
            root.join("Dev/Areas/app/ops/project.yaml"),
            "name: App\nkind: code\n",
        )
        .unwrap();
        Self(root)
    }
}
impl Drop for Vault {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

type Events = Rc<RefCell<Vec<NewProjectEvent>>>;

fn open<'a>(
    cx: &'a mut TestAppContext,
    vault: &Path,
    folder: &str,
    fixed: bool,
) -> (Entity<NewProjectDialog>, &'a mut VisualTestContext, Events) {
    cx.update(|cx| {
        cx.set_reduce_motion(true);
        gpui_component::init(cx);
        crate::project_avatar::set_lucide_names(["server", "folder"].map(str::to_owned), cx);
    });
    let events = Rc::new(RefCell::new(Vec::new()));
    let mut entity = None;
    let vault = vault.to_owned();
    let folder = folder.to_owned();
    let (_, visual) = cx.add_window_view(|window, cx| {
        let dialog = cx.new(|cx| NewProjectDialog::new(&vault, &folder, fixed, window, cx));
        let seen = events.clone();
        cx.subscribe(&dialog, move |_, _, event: &NewProjectEvent, _| {
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

fn drawn(visual: &mut VisualTestContext, selector: &'static str) -> bool {
    visual.update(|window, cx| window.draw(cx).clear(cx));
    visual.debug_bounds(selector).is_some()
}

/// #1475: the name is the folder's, the kind knowledge, and «Create the
/// project in T3» follows the kind: on for code, off for knowledge and
/// hybrid. Only code and hybrid ask where the code is.
#[gpui::test]
fn the_dialog_starts_from_the_folder_and_t3_follows_the_kind(cx: &mut TestAppContext) {
    let vault = Vault::new();
    let (dialog, visual, _) = open(cx, &vault.0, "HomeLab", true);
    dialog.read_with(visual, |d, cx| {
        assert_eq!(d.draft().name, "HomeLab");
        assert_eq!(d.draft().kind, Kind::Knowledge);
        assert!(!d.draft().create_in_t3());
        assert_eq!(d.name.read(cx).value().as_str(), "HomeLab");
        assert_eq!(d.folder(cx), Some(vault.0.join("HomeLab")));
    });
    for selector in [
        "new-project-folder",
        "new-project-name",
        "new-project-kinds",
        "new-project-icon",
        "new-project-t3",
        "new-project-create",
        "new-project-cancel",
    ] {
        assert!(drawn(visual, selector), "{selector}");
    }
    assert!(
        !drawn(visual, "new-project-folder-input"),
        "the folder is fixed"
    );
    assert!(
        !drawn(visual, "new-project-source"),
        "knowledge has no code"
    );

    click(visual, "new-project-kind-0");
    dialog.read_with(visual, |d, _| {
        assert_eq!(d.draft().kind, Kind::Code);
        assert!(d.draft().create_in_t3(), "on for code");
    });
    assert!(drawn(visual, "new-project-source"));
    click(visual, "new-project-kind-2");
    dialog.read_with(visual, |d, _| {
        assert_eq!(d.draft().kind, Kind::Hybrid);
        assert!(!d.draft().create_in_t3(), "off for hybrid");
    });
    click(visual, "new-project-t3");
    dialog.read_with(visual, |d, _| assert!(d.draft().create_in_t3()));
    click(visual, "new-project-kind-1");
    dialog.read_with(visual, |d, _| {
        assert!(d.draft().create_in_t3(), "the user's choice stays");
    });
}

/// «+ New project…» asks for the folder; the name follows it until typed.
#[gpui::test]
fn the_name_follows_the_typed_folder_until_it_is_edited(cx: &mut TestAppContext) {
    let vault = Vault::new();
    let (dialog, visual, _) = open(cx, &vault.0, "HomeLab", false);
    assert!(drawn(visual, "new-project-folder-input"));
    dialog.update_in(visual, |d, window, cx| d.focus_first(window, cx));
    visual.run_until_parked();
    visual.simulate_input("/Mesh");
    visual.run_until_parked();
    dialog.read_with(visual, |d, cx| {
        assert_eq!(d.relative(cx), "HomeLab/Mesh");
        assert_eq!(d.draft().name, "Mesh");
        assert_eq!(d.name.read(cx).value().as_str(), "Mesh");
    });
    dialog.update_in(visual, |d, window, cx| {
        d.name.update(cx, |input, cx| input.focus(window, cx))
    });
    visual.run_until_parked();
    visual.simulate_input(" net");
    visual.run_until_parked();
    dialog.update_in(visual, |d, window, cx| {
        d.folder.update(cx, |input, cx| input.focus(window, cx))
    });
    visual.run_until_parked();
    visual.simulate_input("x");
    visual.run_until_parked();
    dialog.read_with(visual, |d, cx| {
        assert_eq!(d.relative(cx), "HomeLab/Meshx");
        assert_eq!(d.draft().name, "Mesh net", "a typed name stays");
    });
}

/// Create hands the host the folder and the draft; a folder that is a
/// project already, or none, is said in the dialog and nothing is sent.
#[gpui::test]
fn create_sends_the_folder_and_draft_or_says_what_is_wrong(cx: &mut TestAppContext) {
    let vault = Vault::new();
    let (dialog, visual, events) = open(cx, &vault.0, "Dev/Areas/app", false);
    click(visual, "new-project-create");
    assert!(events.borrow().is_empty());
    dialog.read_with(visual, |d, _| {
        assert_eq!(d.error(), Some("Dev/Areas/app is already a project."));
    });
    assert!(drawn(visual, "new-project-error"));

    dialog.update_in(visual, |d, window, cx| {
        d.folder
            .update(cx, |input, cx| input.set_value("Nowhere", window, cx));
        cx.notify();
    });
    click(visual, "new-project-create");
    assert!(events.borrow().is_empty());
    dialog.read_with(visual, |d, _| {
        assert_eq!(d.error(), Some("Nowhere is not a folder in this vault."));
    });

    let icon = ProjectIcon::Lucide {
        name: "server".into(),
        color: IconColor::Blue,
    };
    dialog.update_in(visual, |d, window, cx| {
        d.folder
            .update(cx, |input, cx| input.set_value("HomeLab/Mesh/", window, cx));
        d.name
            .update(cx, |input, cx| input.set_value("  Mesh  ", window, cx));
        d.set_icon(Some(icon.clone()), cx);
    });
    click(visual, "new-project-create");
    let sent = events.borrow().clone();
    let [NewProjectEvent::Create { folder, draft }] = &sent[..] else {
        panic!("one Create: {sent:?}")
    };
    assert_eq!(folder, &vault.0.join("HomeLab").join("Mesh"));
    assert_eq!(draft.name, "Mesh");
    assert_eq!(draft.kind, Kind::Knowledge);
    assert_eq!(draft.icon, Some(icon));
    assert!(!draft.create_in_t3());
    // Busy until the host answers; a second press sends nothing.
    click(visual, "new-project-create");
    assert_eq!(events.borrow().len(), 1);
    dialog.update(visual, |d, cx| d.set_error(Some("disk full".into()), cx));
    dialog.read_with(visual, |d, _| assert_eq!(d.error(), Some("disk full")));
}

/// Choose icon… asks the host for T3's icon dialog; Automatic clears it.
#[gpui::test]
fn the_icon_comes_from_the_shared_icon_dialog(cx: &mut TestAppContext) {
    let vault = Vault::new();
    let (dialog, visual, events) = open(cx, &vault.0, "HomeLab", true);
    assert!(!drawn(visual, "new-project-icon-auto"));
    click(visual, "new-project-icon");
    assert_eq!(*events.borrow(), [NewProjectEvent::ChooseIcon]);
    let icon = ProjectIcon::Emoji {
        emoji: "🛰️".into()
    };
    dialog.update(visual, |d, cx| d.set_icon(Some(icon.clone()), cx));
    assert!(drawn(visual, "new-project-icon-auto"));
    click(visual, "new-project-icon-auto");
    dialog.read_with(visual, |d, _| assert_eq!(d.draft().icon, None));
}

/// A code project with T3 needs a checkout path on the T3 host.
#[gpui::test]
fn a_code_project_for_t3_needs_its_checkout(cx: &mut TestAppContext) {
    let vault = Vault::new();
    let (dialog, visual, events) = open(cx, &vault.0, "HomeLab/Mesh", true);
    click(visual, "new-project-kind-0");
    click(visual, "new-project-create");
    assert!(events.borrow().is_empty());
    dialog.read_with(visual, |d, _| {
        assert!(d.error().is_some_and(|e| e.contains("checkout path")));
    });
    dialog.update_in(visual, |d, window, cx| {
        d.source.update(cx, |input, cx| {
            input.set_value("/home/me/src/mesh", window, cx)
        });
    });
    click(visual, "new-project-create");
    let sent = events.borrow().clone();
    let [NewProjectEvent::Create { draft, .. }] = &sent[..] else {
        panic!("one Create: {sent:?}")
    };
    assert_eq!(draft.kind, Kind::Code);
    assert!(draft.create_in_t3());
    assert_eq!(
        draft.source(),
        Some(okilum_t3::projects::create::Source::Checkout(
            "/home/me/src/mesh".into()
        ))
    );
}
