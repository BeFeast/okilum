//! #2837: «Open in terminal» reaches the folder launcher from every surface,
//! through an application-scoped fake; no real terminal is started.
use super::*;
use crate::project_terminal::{Launches, OpenInTerminal};

const FOLDER: &str = "-rf Папка 'q' $(x)";
const NOTE: &str = "-rf Папка 'q' $(x)/@note ✓.md";

fn launches(visual: &mut VisualTestContext) -> Vec<PathBuf> {
    visual.update(|_, cx| cx.global::<Launches>().0.clone())
}

#[gpui::test]
fn menu_shortcut_and_palette_open_the_items_folder(cx: &mut TestAppContext) {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("vault");
    std::fs::create_dir_all(root.join(FOLDER)).unwrap();
    std::fs::write(root.join(NOTE), "# Note\n").unwrap();
    std::fs::write(root.join("start.md"), "# Start\n").unwrap();
    cx.update(|cx| {
        gpui_component::init(cx);
        bind_keys(cx);
        cx.set_global(Launches::default());
    });
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some(NOTE.into()),
                    index_dir: Some(fixture.path().join("index")),
                    session_directory: Some(fixture.path().join("state")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        reader = Some(entity.clone());
        Root::new(entity, window, cx)
    });
    let reader = reader.unwrap();
    visual.run_until_parked();
    let folder = root.join(FOLDER).canonicalize().unwrap();
    let vault = root.canonicalize().unwrap();

    // Sidebar row menus: a folder opens itself, a file its containing folder.
    for (rel, expected) in [(FOLDER, &folder), ("start.md", &vault)] {
        let item = row_items(&root, rel)
            .into_iter()
            .find_map(|item| match item {
                PopupMenuItem::Item {
                    label,
                    action,
                    handler,
                    ..
                } if label.as_ref() == TERMINAL_LABEL => Some((action, handler)),
                _ => None,
            })
            .expect("row menu offers Open in terminal");
        assert!(item.0.unwrap().partial_eq(&OpenInTerminal), "shortcut hint");
        let before = launches(visual).len();
        visual.update(|window, cx| item.1.unwrap()(&ClickEvent::default(), window, cx));
        assert_eq!(launches(visual)[before..], [expected.clone()], "{rel}");
    }

    // The bound shortcut acts on the open note: its folder.
    reader.update_in(visual, |r, window, cx| window.focus(&r.focus_handle, cx));
    let before = launches(visual).len();
    visual.simulate_keystrokes("alt-cmd-t");
    visual.run_until_parked();
    assert_eq!(launches(visual)[before..], [folder.clone()]);

    // The catalog lists it with its live key for Settings and hints.
    visual.update(|_, cx| {
        let keymap = gpui::Keymap::new(cx.key_bindings().borrow().bindings().cloned().collect());
        let row = reader_shortcuts::rows(&keymap, Os::CURRENT)
            .into_iter()
            .find(|row| row.label == TERMINAL_LABEL)
            .expect("Open in terminal is in the shortcut catalog");
        assert_eq!(row.keys.len(), 1);
        assert!(!reader_shortcuts::all_shortcuts(&OpenInTerminal, cx).is_empty());
    });

    // The palette offers it for the open file and runs the same action.
    reader.update_in(visual, |this, window, cx| {
        this.open_quick_open(false, window, cx);
        this.quick_open
            .input
            .update(cx, |input, cx| input.set_value("in terminal", window, cx));
        this.refresh_quick_open(cx);
    });
    visual.run_until_parked();
    let before = launches(visual).len();
    reader.update_in(visual, |this, window, cx| {
        let ix = this
            .palette_items()
            .iter()
            .position(|(_, item)| matches!(item, quick_open::PaletteItem::OpenInTerminal))
            .expect("palette lists Open in terminal");
        this.open_palette_item(ix, window, cx);
    });
    visual.run_until_parked();
    assert_eq!(launches(visual)[before..], [folder.clone()]);

    // A path outside the vault is refused before any launch.
    let before = launches(visual).len();
    visual.update(|window, cx| run(FileAction::Terminal, &root, "../", window, cx));
    assert_eq!(launches(visual).len(), before);
    assert!(std::fs::read_to_string(root.join(NOTE)).unwrap() == "# Note\n");
}

#[test]
fn a_file_opens_its_parent_and_a_folder_itself() {
    let fixture = tempfile::tempdir().unwrap();
    let folder = fixture.path().join("@Assets");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join("-x.pdf"), b"%PDF").unwrap();
    assert_eq!(
        crate::project_terminal::folder_for(&folder.join("-x.pdf")),
        folder
    );
    assert_eq!(crate::project_terminal::folder_for(&folder), folder);
}
