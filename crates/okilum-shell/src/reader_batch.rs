//! Explicit, serial batches reuse the single-item recovery and revision guards.
use super::*;
use gpui_component::{notification::Notification, WindowExt};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeSet,
};

#[derive(Default)]
pub(super) struct State {
    pub running: bool,
    pub dispatching: bool,
    completion: RefCell<Option<async_channel::Sender<Result<(), String>>>>,
    cancel: Rc<Cell<bool>>,
}

#[derive(Clone, Copy)]
pub(super) enum Kind {
    Move,
    Rename,
    Trash,
}

#[derive(Clone, Debug)]
enum Step {
    Move { from: String, to: String },
    Trash(String),
}
impl Step {
    fn source(&self) -> &str {
        match self {
            Self::Move { from, .. } => from,
            Self::Trash(from) => from,
        }
    }
}

fn validate_plan(root: &Path, steps: &[Step]) -> anyhow::Result<()> {
    anyhow::ensure!(!steps.is_empty(), "Select at least one item.");
    let mut sources = BTreeSet::new();
    let mut targets = BTreeSet::new();
    for step in steps {
        let source = step.source();
        anyhow::ensure!(
            !source.is_empty()
                && Path::new(source)
                    .components()
                    .all(|c| matches!(c, std::path::Component::Normal(_))),
            "Choose items inside this vault."
        );
        anyhow::ensure!(sources.insert(source.to_owned()), "An item occurs twice.");
        reader_files::checked_path(root, source)?;
        if let Step::Move { from, to } = step {
            anyhow::ensure!(
                !to.is_empty()
                    && Path::new(to)
                        .components()
                        .all(|c| matches!(c, std::path::Component::Normal(_))),
                "Choose a name inside this vault."
            );
            anyhow::ensure!(
                from != to && !Path::new(to).starts_with(from),
                "Choose a different name or folder."
            );
            anyhow::ensure!(
                targets.insert(to.clone()),
                "Two items have the same destination."
            );
            anyhow::ensure!(
                !root.join(to).try_exists()?,
                "A destination already exists: {to}"
            );
            let parent = Path::new(to).parent().unwrap_or(Path::new(""));
            anyhow::ensure!(
                reader_files::checked_path(root, &parent.to_string_lossy())?.is_dir(),
                "The destination folder does not exist."
            );
        }
    }
    anyhow::ensure!(
        !sources.iter().any(|source| targets.contains(source)),
        "A destination is another selected item."
    );
    Ok(())
}

struct BatchToast;

impl Reader {
    pub(super) fn complete_batch_item(&self, result: Result<(), String>) {
        if let Some(sender) = self.batch.completion.borrow_mut().take() {
            let _ = sender.try_send(result);
        }
    }

    pub(super) fn render_batch_selection(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let count = self.tree.selected_paths().len();
        if count < 2 && !self.batch.running {
            return None;
        }
        let mut bar = h_flex()
            .px_2()
            .py_1()
            .gap_2()
            .flex_wrap()
            .child(format!("{count} selected"));
        for (id, label, kind) in [
            ("batch-move", "Move…", Kind::Move),
            ("batch-rename", "Rename…", Kind::Rename),
            ("batch-trash", "Trash…", Kind::Trash),
        ] {
            bar = bar.child(
                Button::new(id)
                    .debug_selector(move || id.into())
                    .small()
                    .ghost()
                    .label(label)
                    .disabled(self.batch.running)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_batch_dialog(kind, window, cx)
                    })),
            );
        }
        if self.batch.running {
            let cancel = self.batch.cancel.clone();
            bar = bar.child(
                Button::new("batch-stop")
                    .small()
                    .label("Stop after this item")
                    .on_click(move |_, _, _| cancel.set(true)),
            );
        }
        Some(bar.into_any_element())
    }

    pub(super) fn open_batch_dialog(
        &mut self,
        kind: Kind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.batch.running
            || self.note_move_pending
            || self.trash_pending
            || self.renaming.is_some()
            || self.creation.is_some()
            || window.has_active_dialog(cx)
        {
            self.file_operation_blocked(
                "batch",
                "",
                "Finish the current file operation first.",
                cx,
            );
            return;
        }
        let paths = self.tree.operation_paths();
        if paths.is_empty() {
            return;
        }
        let root = self.vault_root.clone();
        let reader = cx.weak_entity();
        let form = cx.new(|cx| BatchForm::new(reader, root, paths, kind, window, cx));
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(match kind {
                    Kind::Move => "Move selected items",
                    Kind::Rename => "Rename selected items",
                    Kind::Trash => "Move selected items to Trash",
                })
                .width(px(640.))
                .child(form.clone())
        });
    }

    fn run_batch(
        &mut self,
        root: PathBuf,
        steps: Vec<Step>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.vault_root == root,
            "The open vault changed. Select the items again."
        );
        anyhow::ensure!(
            !self.batch.running
                && !self.note_move_pending
                && !self.trash_pending
                && self.renaming.is_none()
                && self.creation.is_none(),
            "Finish the current operation first."
        );
        validate_plan(&root, &steps)?;
        anyhow::ensure!(self.save_source(cx), "Save or recover unsaved edits first.");
        self.batch.running = true;
        self.batch.cancel = Rc::new(Cell::new(false));
        let cancel = self.batch.cancel.clone();
        let total = steps.len();
        cx.spawn_in(window, async move |this, cx| {
            let mut completed = 0;
            let mut stopped = None;
            for step in steps {
                if cancel.get() { stopped = Some("Stopped at your request.".to_owned()); break; }
                let begun = this.update_in_own_window(cx, |reader, window, cx| -> anyhow::Result<_> {
                    anyhow::ensure!(reader.vault_root == root, "The open vault changed.");
                    anyhow::ensure!(!reader.note_move_pending && !reader.trash_pending, "Another file operation is running.");
                    let (send, receive) = async_channel::bounded(1);
                    *reader.batch.completion.borrow_mut() = Some(send);
                    reader.batch.dispatching = true;
                    let result = match &step {
                        Step::Move { from, to } => reader.start_move_preview(&root.join(to), from, window, cx),
                        Step::Trash(from) => { reader.delete_path(from.clone(), window, cx); Ok(()) }
                    };
                    reader.batch.dispatching = false;
                    if let Err(error) = result { reader.complete_batch_item(Err(format!("{error:#}"))); }
                    Ok(receive)
                });
                let outcome = match begun {
                    Ok(Ok(receive)) => receive.recv().await.unwrap_or_else(|_| Err("The operation ended without a completion receipt.".into())),
                    Ok(Err(error)) => Err(format!("{error:#}")),
                    Err(error) => Err(format!("{error:#}")),
                };
                match outcome {
                    Ok(()) => completed += 1,
                    Err(error) => { stopped = Some(format!("{}: {error}", step.source())); break; }
                }
            }
            let _ = this.update_in_own_window(cx, |reader, window, cx| {
                reader.batch.running = false;
                reader.batch.completion.borrow_mut().take();
                let message = match stopped {
                    Some(reason) => format!("{completed} of {total} items completed. {reason} Unstarted items were not changed by this batch. Review recovery for the refused item. Completed moves can be recovered; Trash items can be undone."),
                    None => format!("{completed} items completed. Moves are in Recover link moves; Trash items can be undone."),
                };
                window.push_notification(Notification::new().id::<BatchToast>().message(message).autohide(false), cx);
                cx.notify();
            });
        }).detach();
        cx.notify();
        Ok(())
    }
}

struct BatchForm {
    reader: WeakEntity<Reader>,
    root: PathBuf,
    paths: Vec<String>,
    kind: Kind,
    inputs: Vec<Entity<InputState>>,
    error: Option<String>,
}
impl BatchForm {
    fn new(
        reader: WeakEntity<Reader>,
        root: PathBuf,
        paths: Vec<String>,
        kind: Kind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let inputs = match kind {
            Kind::Move => vec![cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Destination folder, or empty for vault root")
            })],
            Kind::Rename => paths
                .iter()
                .map(|path| {
                    cx.new(|cx| {
                        let mut input = InputState::new(window, cx);
                        input.set_value(
                            Path::new(path)
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .to_string(),
                            window,
                            cx,
                        );
                        input
                    })
                })
                .collect(),
            Kind::Trash => vec![],
        };
        Self {
            reader,
            root,
            paths,
            kind,
            inputs,
            error: None,
        }
    }

    fn plan(&self, cx: &App) -> anyhow::Result<Vec<Step>> {
        let mut steps = Vec::new();
        for (index, from) in self.paths.iter().enumerate() {
            let step = match self.kind {
                Kind::Trash => Step::Trash(from.clone()),
                Kind::Move | Kind::Rename => {
                    let meta = std::fs::symlink_metadata(self.root.join(from))?;
                    anyhow::ensure!(meta.is_dir() || Path::new(from).extension().is_some_and(|e| e.eq_ignore_ascii_case("md")), "Move and rename currently support notes and folders. Attachments can be moved with their folder or sent to Trash.");
                    let to = match self.kind {
                        Kind::Move => PathBuf::from(self.inputs[0].read(cx).value().trim())
                            .join(Path::new(from).file_name().unwrap_or_default()),
                        Kind::Rename => {
                            let value = self.inputs[index].read(cx).value();
                            let name = value.trim();
                            anyhow::ensure!(
                                !name.is_empty()
                                    && Path::new(name).components().count() == 1
                                    && !name.contains(['/', '\\']),
                                "Enter one filename for each item."
                            );
                            Path::new(from).parent().unwrap_or(Path::new("")).join(name)
                        }
                        Kind::Trash => unreachable!(),
                    };
                    let mut to = to;
                    if !meta.is_dir()
                        && !to.extension().is_some_and(|e| e.eq_ignore_ascii_case("md"))
                    {
                        let name = to.file_name().unwrap_or_default().to_string_lossy();
                        to.set_file_name(format!("{name}.md"));
                    }
                    let to = okilum_core::vault::note_path(&to);
                    if &to == from {
                        continue;
                    }
                    Step::Move {
                        from: from.clone(),
                        to,
                    }
                }
            };
            steps.push(step);
        }
        validate_plan(&self.root, &steps)?;
        Ok(steps)
    }
}
impl Render for BatchForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut form = v_flex().gap_3().child(format!("{} items. Selected folders include their contents once. Operations run in order and stop on the first refusal or cancellation. Existing link previews, recovery and Trash Undo still apply.", self.paths.len()));
        if matches!(self.kind, Kind::Move) {
            form = form.child(Input::new(&self.inputs[0]));
        }
        form = form.child(
            v_flex()
                .id("batch-items")
                .max_h(px(320.))
                .overflow_y_scroll()
                .gap_2()
                .children(self.paths.iter().enumerate().map(|(index, path)| {
                    v_flex()
                        .gap_1()
                        .child(path.clone())
                        .when(matches!(self.kind, Kind::Rename), |row| {
                            row.child(Input::new(&self.inputs[index]))
                        })
                })),
        );
        form.when_some(self.error.clone(), |form, error| {
            form.child(div().text_color(brand::palette(cx).danger).child(error))
        })
        .child(
            h_flex()
                .justify_end()
                .gap_2()
                .child(
                    Button::new("batch-cancel")
                        .label("Cancel")
                        .ghost()
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                )
                .child(
                    Button::new("batch-apply")
                        .debug_selector(|| "batch-apply".into())
                        .primary()
                        .label(match self.kind {
                            Kind::Move => "Move items",
                            Kind::Rename => "Rename items",
                            Kind::Trash => "Move items to Trash",
                        })
                        .on_click(cx.listener(|this, _, window, cx| {
                            let result = this.plan(cx).and_then(|steps| {
                                this.reader.update(cx, |reader, cx| {
                                    reader.run_batch(this.root.clone(), steps, window, cx)
                                })?
                            });
                            match result {
                                Ok(()) => window.close_dialog(cx),
                                Err(error) => {
                                    this.error = Some(format!("{error:#}"));
                                    cx.notify();
                                }
                            }
                        })),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn issue2681_preflight_refuses_collisions_traversal_and_occupied_targets() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::write(root.join("A.md"), b"A\r\n").unwrap();
        std::fs::write(root.join("B.md"), b"B\r\n").unwrap();
        std::fs::create_dir(root.join("Dest")).unwrap();
        let move_to = |from: &str, to: &str| Step::Move {
            from: from.into(),
            to: to.into(),
        };
        assert!(validate_plan(
            root,
            &[move_to("A.md", "Dest/A.md"), move_to("B.md", "Dest/B.md")]
        )
        .is_ok());
        for steps in [
            vec![move_to("A.md", "B.md")],
            vec![move_to("A.md", "Dest/X.md"), move_to("B.md", "Dest/X.md")],
            vec![move_to("A.md", "../outside.md")],
            vec![Step::Trash("".into())],
        ] {
            assert!(validate_plan(root, &steps).is_err());
        }
        assert_eq!(std::fs::read(root.join("A.md")).unwrap(), b"A\r\n");
        assert_eq!(std::fs::read(root.join("B.md")).unwrap(), b"B\r\n");
    }

    #[gpui::test]
    fn issue2681_mounted_modified_clicks_select_without_opening_and_show_batch_actions(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
            cx.set_reduce_motion(true);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        for path in ["A.md", "B.md", "C.md", "Home.md"] {
            std::fs::write(root.join(path), format!("# {path}\r\n")).unwrap();
        }
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("Home.md".into()),
                        index_dir: Some(temp.path().join("index")),
                        session_directory: Some(temp.path().join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.simulate_resize(size(px(1200.), px(900.)));
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            r.reveal_in_tree("A.md", window, cx);
        });
        visual.run_until_parked();
        let toggle = Modifiers {
            control: !cfg!(target_os = "macos"),
            platform: cfg!(target_os = "macos"),
            ..Default::default()
        };
        let a = visual.debug_bounds("tree-row-A.md").expect("A visible");
        visual.simulate_click(a.center(), Modifiers::default());
        visual.run_until_parked();
        let b = visual.debug_bounds("tree-row-B.md").expect("B visible");
        visual.simulate_click(b.center(), toggle);
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert_eq!(r.tree.selected_paths(), ["A.md", "B.md"]);
            assert_eq!(r.current_rel, "A.md", "modified click must not open B");
        });
        for id in ["batch-move", "batch-rename", "batch-trash"] {
            assert!(visual.debug_bounds(id).is_some(), "missing {id}");
        }
        let c = visual.debug_bounds("tree-row-C.md").unwrap();
        visual.simulate_click(
            c.center(),
            Modifiers {
                shift: true,
                ..Default::default()
            },
        );
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert_eq!(r.tree.selected_paths(), ["B.md", "C.md"])
        });
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        reader.read_with(visual, |r, _| assert!(r.tree.selected_paths().is_empty()));
    }

    #[gpui::test]
    fn issue2681_batch_moves_rename_recovery_and_locked_item_stop(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
            cx.set_reduce_motion(true);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir_all(root.join("Dest")).unwrap();
        let root = root.canonicalize().unwrap();
        let bytes = b"# Canonical\r\n\r\nSynthetic unchanged bytes.\r\n";
        for path in ["A.md", "B.md", "C.md", "Home.md"] {
            std::fs::write(root.join(path), bytes).unwrap();
        }
        let state = temp.path().join("state");
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("A.md".into()),
                        index_dir: Some(temp.path().join("index")),
                        session_directory: Some(state.clone()),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        let move_to = |from: &str, to: &str| Step::Move {
            from: from.into(),
            to: to.into(),
        };
        reader.update_in(visual, |r, window, cx| {
            r.run_batch(
                root.clone(),
                vec![move_to("A.md", "Dest/A.md"), move_to("B.md", "Dest/B.md")],
                window,
                cx,
            )
            .unwrap()
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert!(!r.batch.running && !r.note_move_pending);
            assert_eq!(
                r.current_rel, "Dest/A.md",
                "the open first item follows its move while the second item still completes"
            );
        });
        for path in ["Dest/A.md", "Dest/B.md"] {
            assert_eq!(std::fs::read(root.join(path)).unwrap(), bytes);
        }
        assert!(!root.join("A.md").exists() && !root.join("B.md").exists());
        let operations = okilum_core::link_rewrite::Operation::list(&state, &root).unwrap();
        assert_eq!(
            operations.operations.len(),
            2,
            "both moves retain their recovery journals"
        );
        reader.update_in(visual, |r, window, cx| {
            r.run_batch(
                root.clone(),
                vec![
                    move_to("Dest/A.md", "Dest/One.md"),
                    move_to("Dest/B.md", "Dest/Two.md"),
                ],
                window,
                cx,
            )
            .unwrap()
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert!(!r.batch.running && !r.note_move_pending);
            assert_eq!(
                r.current_rel, "Dest/One.md",
                "the open item follows batch rename"
            );
        });
        for path in ["Dest/One.md", "Dest/Two.md"] {
            assert_eq!(std::fs::read(root.join(path)).unwrap(), bytes);
        }
        let guard = okilum_core::file_editor::FileEditor::reserve_destination(
            &root.join("Dest/Two.md"),
            &state.join("editor-drafts"),
        )
        .unwrap();
        reader.update_in(visual, |r, window, cx| {
            r.run_batch(
                root.clone(),
                vec![
                    move_to("Dest/One.md", "One.md"),
                    move_to("Dest/Two.md", "Two.md"),
                    move_to("C.md", "Dest/C.md"),
                ],
                window,
                cx,
            )
            .unwrap()
        });
        visual.run_until_parked();
        assert!(
            root.join("One.md").exists(),
            "first confirmed item completed"
        );
        assert!(
            root.join("Dest/Two.md").exists() && !root.join("Two.md").exists(),
            "locked item kept"
        );
        assert!(
            root.join("C.md").exists() && !root.join("Dest/C.md").exists(),
            "batch stopped before third item"
        );
        reader.read_with(visual, |r, _| {
            assert!(!r.batch.running && !r.note_move_pending);
            assert_eq!(r.current_rel, "One.md");
        });
        drop(guard);
        reader.update_in(visual, |r, window, cx| {
            r.run_batch(
                root.clone(),
                vec![Step::Trash("One.md".into()), Step::Trash("C.md".into())],
                window,
                cx,
            )
            .unwrap()
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert!(!r.batch.running && !r.trash_pending);
            assert!(r.current_rel.is_empty(), "successful explicit Trash closes the current document without blocking the second item");
        });
        assert!(
            !root.join("One.md").exists() && !root.join("C.md").exists(),
            "both files reached Trash"
        );
        for _ in 0..2 {
            reader.update_in(visual, |r, window, cx| r.undo_last_trash(window, cx));
            visual.run_until_parked();
        }
        for path in ["One.md", "C.md"] {
            assert_eq!(
                std::fs::read(root.join(path)).unwrap(),
                bytes,
                "Undo restored {path}"
            );
        }
        assert_eq!(std::fs::read(root.join("Home.md")).unwrap(), bytes);
    }
}
