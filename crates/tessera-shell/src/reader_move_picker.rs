//! Folder destinations from the loaded inventory; writes use the shared move pipeline.
use super::*;
use gpui_component::WindowExt;
use tessera_core::vault::EntryKind;

gpui::actions!(
    tree_move,
    [
        MoveToFolder,
        FolderNext,
        FolderPrevious,
        FolderAccept,
        CloseFolderPicker
    ]
);

pub(super) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("enter", FolderAccept, Some("MoveFolderPicker > Input")),
        KeyBinding::new("down", FolderNext, Some("MoveFolderPicker > Input")),
        KeyBinding::new("up", FolderPrevious, Some("MoveFolderPicker > Input")),
        KeyBinding::new(
            "escape",
            CloseFolderPicker,
            Some("MoveFolderPicker > Input"),
        ),
    ]);
}

#[derive(Clone)]
pub(super) struct TreeDrag {
    root: PathBuf,
    from: String,
    label: String,
}
impl Render for TreeDrag {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = brand::palette(cx);
        div()
            .px_3()
            .py_2()
            .rounded_md()
            .bg(p.surface)
            .text_color(p.text)
            .shadow_md()
            .child(self.label.clone())
    }
}

fn folder_label(path: &str) -> String {
    if path.is_empty() {
        "Vault root".into()
    } else {
        path.replace('/', " › ")
    }
}
fn valid_destination(from: &str, folder: &str) -> bool {
    !from.is_empty()
        && !Path::new(folder).starts_with(from)
        && Path::new(from).parent().unwrap_or(Path::new("")) != Path::new(folder)
}

/// A folder row (or the Folders header for vault root) is the only drop target.
pub(super) fn drop_target(
    row: Stateful<Div>,
    reader: WeakEntity<Reader>,
    root: PathBuf,
    folder: String,
) -> Stateful<Div> {
    let allowed_root = root.clone();
    let allowed_folder = folder.clone();
    let highlight_root = root;
    let highlight_folder = folder.clone();
    row.can_drop(move |item, _, _| {
        item.downcast_ref::<TreeDrag>().is_some_and(|drag| {
            drag.root == allowed_root && valid_destination(&drag.from, &allowed_folder)
        })
    })
    .drag_over::<TreeDrag>(move |style, drag, _, cx| {
        if drag.root == highlight_root && valid_destination(&drag.from, &highlight_folder) {
            style
                .bg(brand::palette(cx).selected)
                .border_1()
                .border_color(brand::palette(cx).focus)
        } else {
            style
        }
    })
    .on_drop(move |drag: &TreeDrag, window, cx| {
        let _ = reader.update(cx, |r, cx| {
            if let Err(error) = r.move_to_folder(&drag.root, &drag.from, &folder, window, cx) {
                eprintln!("Tree move failed: {error:#}");
                reader_toast::error(move_error(&error), window, cx);
            }
        });
    })
}
pub(super) fn draggable(
    row: Stateful<Div>,
    root: PathBuf,
    from: String,
    label: String,
) -> Stateful<Div> {
    row.on_drag(TreeDrag { root, from, label }, |drag, _, _, cx| {
        cx.new(|_| drag.clone())
    })
}
fn move_error(error: &anyhow::Error) -> &'static str {
    let text = error.to_string();
    if text.contains("exist") || text.contains("occupied") {
        "This folder already contains that name."
    } else if text.contains("unsaved") || text.contains("draft") {
        "Save or recover unsaved edits first."
    } else if text.contains("operation") {
        "Finish the current operation first."
    } else {
        "Cannot move this item here. Choose another folder or try again."
    }
}

#[derive(Default)]
pub(super) struct PickerState {
    active: Option<(Entity<FolderPicker>, Point<Pixels>)>,
    recent: std::collections::BTreeMap<PathBuf, Vec<String>>,
}

impl Reader {
    pub(super) fn render_move_picker(&self) -> Option<impl IntoElement> {
        self.move_picker.active.as_ref().map(|(picker, position)| {
            anchored()
                .position(*position)
                .snap_to_window_with_margin(px(12.))
                .child(picker.clone())
        })
    }

    pub(super) fn move_to_folder(
        &mut self,
        root: &Path,
        from: &str,
        folder: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(root == self.vault_root, "The open vault changed");
        anyhow::ensure!(
            [from, folder].iter().all(|p| Path::new(p)
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)))),
            "Choose a destination inside this vault"
        );
        anyhow::ensure!(
            !self.note_move_pending
                && !self.trash_pending
                && self.renaming.is_none()
                && self.creation.is_none(),
            "Finish the current operation first"
        );
        anyhow::ensure!(valid_destination(from, folder), "Choose another folder");
        let meta = std::fs::symlink_metadata(root.join(from))?;
        anyhow::ensure!(
            meta.is_dir()
                || (meta.is_file()
                    && Path::new(from)
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("md"))),
            "Choose a note or folder"
        );
        anyhow::ensure!(
            std::fs::symlink_metadata(root.join(folder))?.is_dir(),
            "Destination folder changed"
        );
        let name = Path::new(from)
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("Missing item name"))?;
        self.start_move_preview(&root.join(folder).join(name), from, window, cx)
    }
    pub(super) fn choose_move_folder(
        &mut self,
        from: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx)
            || from.is_empty()
            || self.note_move_pending
            || self.trash_pending
            || self.renaming.is_some()
            || self.creation.is_some()
        {
            return;
        }
        let mut folders = vec![String::new()];
        folders.extend(
            self.vault
                .entries
                .iter()
                .filter(|e| e.kind == EntryKind::Directory)
                .filter(|e| self.sidebar.show_hidden || !reader_tree::hidden(&e.path))
                .map(|e| e.path.clone()),
        );
        folders.retain(|folder| valid_destination(&from, folder));
        folders.sort();
        folders.dedup();
        if let Some(recent) = self.move_picker.recent.get(&self.vault_root) {
            folders.sort_by_key(|folder| {
                recent
                    .iter()
                    .position(|p| p == folder)
                    .unwrap_or(usize::MAX)
            });
        }
        let reader = cx.weak_entity();
        let root = self.vault_root.clone();
        let picker = cx.new(|cx| FolderPicker::new(reader, root, from, folders, window, cx));
        self.move_picker.active = Some((picker, window.mouse_position()));
        cx.notify();
    }
}
struct FolderPicker {
    reader: WeakEntity<Reader>,
    root: PathBuf,
    from: String,
    folders: Vec<String>,
    matches: Vec<String>,
    selected: usize,
    scroll: ScrollHandle,
    focus_on_mount: bool,
    input: Entity<InputState>,
    error: Option<&'static str>,
    _subscription: Subscription,
}
impl FolderPicker {
    fn new(
        reader: WeakEntity<Reader>,
        root: PathBuf,
        from: String,
        folders: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search folders…"));
        let subscription =
            cx.subscribe_in(&input, window, |this, _, event, window, cx| match event {
                InputEvent::Change => {
                    this.filter(cx);
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.accept(window, cx),
                _ => {}
            });
        Self {
            reader,
            root,
            from,
            matches: folders.iter().take(100).cloned().collect(),
            folders,
            selected: 0,
            scroll: ScrollHandle::new(),
            focus_on_mount: true,
            input,
            error: None,
            _subscription: subscription,
        }
    }
    fn filter(&mut self, cx: &App) {
        let query = self.input.read(cx).value().to_lowercase();
        self.matches = self
            .folders
            .iter()
            .filter(|f| {
                f.to_lowercase().contains(&query) || folder_label(f).to_lowercase().contains(&query)
            })
            .take(100)
            .cloned()
            .collect();
        self.selected = 0;
        self.scroll.scroll_to_top_of_item(0);
        self.error = None;
    }
    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        if !self.matches.is_empty() {
            self.selected = self
                .selected
                .saturating_add_signed(delta)
                .min(self.matches.len() - 1);
        }
        self.scroll.scroll_to_item(self.selected);
        cx.notify();
    }
    fn close(&self, restore_focus: bool, window: &mut Window, cx: &mut Context<Self>) {
        let _ = self.reader.update(cx, |reader, cx| {
            reader.move_picker.active = None;
            if restore_focus {
                reader.focus_handle.focus(window, cx);
            }
            cx.notify();
        });
    }

    fn accept(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(folder) = self.matches.get(self.selected) else {
            return;
        };
        let result = self.reader.update(cx, |r, cx| {
            r.move_to_folder(&self.root, &self.from, folder, window, cx)
        });
        match result {
            Ok(Ok(())) => {
                let _ = self.reader.update(cx, |reader, _| {
                    let recent = reader
                        .move_picker
                        .recent
                        .entry(self.root.clone())
                        .or_default();
                    recent.retain(|p| p != folder);
                    recent.insert(0, folder.clone());
                    recent.truncate(8);
                });
                self.close(true, window, cx);
            }
            Ok(Err(error)) => {
                eprintln!("Folder move failed: {error:#}");
                self.error = Some(move_error(&error));
                cx.notify();
            }
            Err(_) => {
                self.error = Some("The note window closed.");
                cx.notify();
            }
        }
    }
}
impl Render for FolderPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.focus_on_mount) {
            let input = self.input.clone();
            window.defer(cx, move |window, cx| {
                input.update(cx, |input, cx| input.focus(window, cx))
            });
        }
        let p = brand::palette(cx);
        v_flex()
            .id("move-folder-popover")
            .debug_selector(|| "move-folder-popover".into())
            .occlude()
            .w(px(360.))
            .p_2()
            .rounded_lg()
            .shadow_lg()
            .bg(p.surface)
            .text_color(p.text)
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close(false, window, cx)))
            .key_context("MoveFolderPicker")
            .gap_2()
            .on_action(cx.listener(|this, _: &FolderAccept, window, cx| this.accept(window, cx)))
            .on_action(cx.listener(|this, _: &FolderNext, _, cx| this.step(1, cx)))
            .on_action(cx.listener(|this, _: &FolderPrevious, _, cx| this.step(-1, cx)))
            .on_action(
                cx.listener(|this, _: &CloseFolderPicker, window, cx| this.close(true, window, cx)),
            )
            .child(Input::new(&self.input).prefix(Icon::new(IconName::Search).small()))
            .when_some(self.error, |d, error| {
                d.child(div().text_size(px(12.)).text_color(p.danger).child(error))
            })
            .child(
                v_flex()
                    .id("move-folder-results")
                    .track_scroll(&self.scroll)
                    .max_h(px(280.))
                    .overflow_y_scroll()
                    .when(self.matches.is_empty(), |d| {
                        d.child(div().py_2().text_color(p.text_muted).child("—"))
                    })
                    .children(self.matches.iter().enumerate().map(|(i, folder)| {
                        h_flex()
                            .id(("move-folder", i))
                            .debug_selector(move || format!("move-folder-{i}"))
                            .px_2()
                            .py_2()
                            .gap_2()
                            .rounded_md()
                            .cursor_pointer()
                            .when(i == self.selected, |d| d.bg(p.selected))
                            .hover(move |d| d.bg(p.selected))
                            .child(Icon::new(IconName::Folder).small())
                            .child(div().flex_1().min_w_0().child(folder_label(folder)))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.selected = i;
                                this.accept(window, cx);
                            }))
                    })),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn destinations_exclude_self_descendants_and_current_parent() {
        assert!(!valid_destination("Source", "Source"));
        assert!(!valid_destination("Source", "Source/Child"));
        assert!(!valid_destination("Source/Note.md", "Source"));
        assert!(!valid_destination("Note.md", ""));
        assert!(valid_destination("Source", "Source archive"));
        assert!(valid_destination("Source/Note.md", ""));
    }

    #[gpui::test]
    fn picker_and_tree_drop_use_link_move_and_exact_undo(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            gpui_component::init(cx);
            super::super::bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir_all(root.join("Source/Child")).unwrap();
        std::fs::create_dir(root.join("Target")).unwrap();
        std::fs::write(root.join("Source/Note.md"), "# Note\r\n").unwrap();
        std::fs::write(root.join("Source/data.bin"), [0, 255, 7]).unwrap();
        std::fs::write(root.join("Start.md"), "[[Source/Note]]\n").unwrap();
        let root = root.canonicalize().unwrap();
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("Start.md".into()),
                        index_dir: Some(temp.path().join("index")),
                        session_directory: Some(temp.path().join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        let reader = entity.unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            r.choose_move_folder("Source/Note.md".into(), window, cx);
            assert!(
                !window.has_active_dialog(cx),
                "Move picker must not open a modal"
            );
        });
        visual.run_until_parked();
        visual.simulate_input("Target");
        visual.simulate_keystrokes("escape");
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            visual.debug_bounds("move-folder-0").is_none(),
            "Escape must close the picker"
        );
        assert!(root.join("Source/Note.md").exists());
        assert!(!root.join("Target/Note.md").exists());
        reader.update_in(visual, |r, window, cx| {
            r.choose_move_folder("Source/Note.md".into(), window, cx);
            assert!(
                !window.has_active_dialog(cx),
                "Move picker must not open a modal"
            );
        });
        visual.run_until_parked();
        visual.simulate_input("Target");
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        assert!(root.join("Target/Note.md").exists());
        assert_eq!(
            std::fs::read_to_string(root.join("Start.md")).unwrap(),
            "[[Target/Note]]\n"
        );
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let undo = visual.debug_bounds("undo-move").expect("move Undo");
        visual.simulate_click(undo.center(), Modifiers::default());
        visual.run_until_parked();
        assert_eq!(
            std::fs::read(root.join("Source/Note.md")).unwrap(),
            b"# Note\r\n"
        );
        reader.update_in(visual, |r, window, cx| {
            assert!(r
                .move_to_folder(&root, "Source", "Source/Child", window, cx)
                .is_err());
            assert!(r
                .move_to_folder(temp.path(), "Source", "Target", window, cx)
                .is_err());
            r.reveal_in_tree("Source", window, cx);
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let source = visual
            .debug_bounds("tree-row-Source")
            .expect("source folder")
            .center();
        let target = visual
            .debug_bounds("tree-row-Target")
            .expect("destination folder")
            .center();
        visual.simulate_mouse_down(source, MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_move(
            source + point(px(10.), px(0.)),
            Some(MouseButton::Left),
            Modifiers::default(),
        );
        visual.simulate_mouse_move(target, Some(MouseButton::Left), Modifiers::default());
        visual.simulate_mouse_up(target, MouseButton::Left, Modifiers::default());
        visual.run_until_parked();
        assert!(
            !root.join("Source").exists(),
            "the actual drop must move the folder"
        );
        assert_eq!(
            std::fs::read(root.join("Target/Source/data.bin")).unwrap(),
            [0, 255, 7]
        );
        assert_eq!(
            std::fs::read_to_string(root.join("Start.md")).unwrap(),
            "[[Source/Note]]\n"
        );
        reader.read_with(visual, |r, _| {
            assert_eq!(
                r.vault.resolve_from("Source/Note", "Start.md").path(),
                Some("Target/Source/Note.md")
            );
        });
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let undo = visual.debug_bounds("undo-move").expect("folder move Undo");
        visual.simulate_click(undo.center(), Modifiers::default());
        // Check before background index/watcher work can repair the tree. The
        // open note is unrelated to the moved folder (the QA reproduction).
        reader.read_with(visual, |r, _| {
            assert_eq!(r.current_rel, "Start.md");
            assert!(r.tree.rows.iter().any(|row| row.path == "Source"));
            assert!(!r.tree.rows.iter().any(|row| row.path == "Target/Source"));
        });
        visual.run_until_parked();
        // Recent destinations outrank alphabetical entries; outside click dismisses
        // without intercepting the rest of the window or opening a modal.
        reader.update_in(visual, |r, window, cx| {
            r.choose_move_folder("Source/Note.md".into(), window, cx);
            let picker = &r.move_picker.active.as_ref().unwrap().0;
            assert_eq!(picker.read(cx).matches.first().unwrap(), "Target");
            assert!(!window.has_active_dialog(cx));
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let bounds = visual.debug_bounds("move-folder-popover").unwrap();
        let outside = if bounds.origin.x > px(10.) {
            point(px(1.), px(1.))
        } else {
            point(bounds.right() + px(10.), px(1.))
        };
        visual.simulate_click(outside, Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |r, _| assert!(r.move_picker.active.is_none()));
        // Verify actual arrow bindings with multiple choices, not a one-result query.
        let picker = reader.update_in(visual, |r, window, cx| {
            let picker = cx.new(|cx| {
                FolderPicker::new(
                    reader.downgrade(),
                    root.clone(),
                    "Source/Note.md".into(),
                    vec!["Target".into(), "Target alternate".into()],
                    window,
                    cx,
                )
            });
            r.move_picker.active = Some((picker.clone(), point(px(20.), px(60.))));
            cx.notify();
            picker
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("down");
        assert_eq!(picker.read_with(visual, |p, _| p.selected), 1);
        visual.simulate_keystrokes("up");
        assert_eq!(picker.read_with(visual, |p, _| p.selected), 0);
        visual.simulate_keystrokes("escape");
        assert!(root.join("Source/Child").is_dir());
        assert_eq!(
            std::fs::read_to_string(root.join("Start.md")).unwrap(),
            "[[Source/Note]]\n"
        );
    }
}
