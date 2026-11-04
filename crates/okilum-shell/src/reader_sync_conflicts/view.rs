use super::*;
use crate::{brand, reader_open, reader_settings_sync, Reader};
use gpui::{prelude::*, *};
use gpui_base::ElementExt as _;
use gpui_component::{button::Button, h_flex, v_flex, Disableable, Sizable, WindowExt};

pub(crate) fn open(
    root: PathBuf,
    state: PathBuf,
    owner: Option<WeakEntity<Reader>>,
    preferred: Option<PathBuf>,
    window: &mut Window,
    cx: &mut App,
) {
    let view = cx.new(|cx| InventoryView::new(root, state, owner, cx));
    view.update(cx, |view, _| view.preferred = preferred);
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .title("Sync conflicts")
            .width(px(1100.))
            .child(view.clone())
    });
}

pub(crate) struct InventoryView {
    root: PathBuf,
    state: PathBuf,
    owner: Option<WeakEntity<Reader>>,
    copies: Vec<PathBuf>,
    preferred: Option<PathBuf>,
    complete: bool,
    pending: usize,
    busy: bool,
    page: usize,
    generation: u64,
    panel: Option<ConflictPanel>,
    message: String,
}
impl InventoryView {
    pub(crate) fn new(
        root: PathBuf,
        state: PathBuf,
        owner: Option<WeakEntity<Reader>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            root,
            state,
            owner,
            copies: vec![],
            preferred: None,
            complete: false,
            pending: 0,
            busy: false,
            page: 0,
            generation: 0,
            panel: None,
            message: String::new(),
        };
        this.refresh(cx);
        this
    }
    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.generation += 1;
        self.panel = None;
        let generation = self.generation;
        let root = self.root.clone();
        let state = self.state.join("sync-conflicts");
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let root = root.canonicalize()?;
                    let inventory = conflicts::inspect(&root)?;
                    let history = conflicts::history(&state, &root)?;
                    Ok::<_, anyhow::Error>((root, inventory, history))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if generation != this.generation {
                    return;
                }
                this.busy = false;
                match result {
                    Ok((root, inventory, history)) => {
                        this.root = root;
                        this.copies = inventory.copies;
                        this.complete = inventory.complete;
                        this.pending = history.entries.iter().filter(|entry| entry.pending).count();
                        this.message = history.warnings.join("\n");
                        this.page = 0;
                    }
                    Err(error) => {
                        this.complete = false;
                        this.message = format!("Cannot inspect conflicts: {error}");
                    }
                }
                if let Some(copy) = this.preferred.take() {
                    this.preview(copy, cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn preview(&mut self, copy: PathBuf, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.generation += 1;
        let generation = self.generation;
        let root = self.root.clone();
        let controller = reader_settings_sync::active_controller(cx);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let mut panel = ConflictPanel::open(&root, &copy)?;
                    if let Some(controller) = controller {
                        for pane in [&mut panel.comparison.this, &mut panel.comparison.that] {
                            let bytes = if pane.path == panel.preview.this().path() {
                                panel.preview.this().bytes()
                            } else {
                                panel.preview.that().bytes()
                            };
                            if let Ok(text) = std::str::from_utf8(bytes) {
                                if let Ok(Some(name)) = controller.file_version_origin(
                                    &root,
                                    &pane.path.to_string_lossy(),
                                    text,
                                ) {
                                    pane.provenance = format!(
                                        "Device: {name} · {}",
                                        pane.provenance
                                            .replace("Origin device unknown.", "")
                                            .replace(
                                                "Device unknown; suffix is random.",
                                                "Suffix is random."
                                            )
                                            .replace(
                                                "timezone/device name unverified",
                                                "timezone unverified"
                                            )
                                    );
                                }
                            }
                        }
                    }
                    Ok::<_, anyhow::Error>(panel)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.busy = false;
                match result {
                    Ok(panel) => {
                        this.panel = Some(panel);
                        this.message.clear();
                    }
                    Err(error) => {
                        this.message = format!(
                            "Cannot preview this conflict: {error}. Both files are retained."
                        )
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn dirty(&self, cx: &App) -> bool {
        let mut readers = reader_open::readers(cx);
        if let Some(owner) = self.owner.as_ref().and_then(WeakEntity::upgrade) {
            if !readers.contains(&owner) {
                readers.push(owner);
            }
        }
        readers.iter().any(|reader| {
            let reader = reader.read(cx);
            // Refresh canonicalizes the inventory root on its worker before
            // enabling actions; Reader roots use that same identity.
            reader.vault_root == self.root && reader.source_is_dirty(cx)
        })
    }
    fn apply(&mut self, choice: Choice, cx: &mut Context<Self>) {
        let dirty = self.dirty(cx);
        let recovery = self.state.join("sync-conflicts");
        let Some(panel) = self.panel.as_mut() else {
            return;
        };
        let Some(job) = panel.begin_apply(choice, &recovery, dirty) else {
            cx.notify();
            return;
        };
        self.busy = true;
        let generation = self.generation;
        let root = self.root.clone();
        let drafts = self.state.join("editor-drafts");
        cx.spawn(async move |this, cx| {
            let (completion, history) = cx
                .background_executor()
                .spawn(async move {
                    let prepare = || -> anyhow::Result<()> {
                        // A draft in another process is equally authoritative. Refuse
                        // rather than letting a clean window hide its unsaved version.
                        for version in [job.preview.this(), job.preview.that()] {
                            anyhow::ensure!(
                                !okilum_core::file_editor::FileEditor::has_unsaved_draft(
                                    &root.join(version.path()),
                                    &drafts
                                )?,
                                "Unsaved draft exists; use Recover notes first"
                            );
                        }
                        conflicts::prepare_recovery(&recovery)?;
                        Ok(())
                    };
                    let completion = match prepare() {
                        Ok(()) => job.run(),
                        Err(error) => Completion {
                            id: job.id,
                            result: Err(error),
                        },
                    };
                    // NeedsAttention can be terminal (failed completion marker)
                    // or recoverable (partial capture). Use durable authority,
                    // rather than labelling every error an interrupted decision.
                    let history = conflicts::history(&recovery, &root);
                    (completion, history)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if generation != this.generation {
                    return;
                }
                this.busy = false;
                if let Some(panel) = this.panel.as_mut() {
                    panel.finish(completion);
                }
                match history {
                    Ok(history) => {
                        this.pending = history.entries.iter().filter(|entry| entry.pending).count();
                        this.message = history.warnings.join("\n");
                    }
                    Err(error) => {
                        this.message = format!("Cannot refresh interrupted decisions: {error}");
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn recover(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if self.dirty(cx) {
            self.message =
                "Save or close unsaved drafts before recovering interrupted resolutions.".into();
            cx.notify();
            return;
        }
        self.busy = true;
        let root = self.root.clone();
        let state = self.state.join("sync-conflicts");
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let messages = conflicts::recover_interrupted(&state, &root)?;
                    let remaining = conflicts::history(&state, &root)?
                        .entries
                        .iter()
                        .filter(|entry| entry.pending)
                        .count();
                    let inventory = conflicts::inspect(&root)?;
                    Ok::<_, anyhow::Error>((messages, remaining, inventory))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                this.message = match result {
                    Ok((messages, remaining, inventory)) => {
                        this.pending = remaining;
                        this.copies = inventory.copies;
                        this.complete = inventory.complete;
                        this.panel = None;
                        this.page = 0;
                        if messages.is_empty() {
                            "No interrupted resolutions.".into()
                        } else {
                            messages.join("\n")
                        }
                    }
                    Err(error) => format!("Cannot recover: {error}"),
                };
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
fn pane(pane: &Pane, id: &'static str, cx: &App) -> AnyElement {
    let p = brand::palette(cx);
    v_flex()
        .flex_1()
        .min_w_0()
        .gap_2()
        .child(pane.path.display().to_string())
        .child(div().text_sm().child(pane.provenance.clone()))
        .child(div().text_sm().child(pane.byte_encoding))
        .child(
            v_flex()
                .id(id)
                .h(px(320.))
                .overflow_scroll()
                .text_sm()
                .font_family(crate::reader_fonts::code(cx))
                .children(pane.lines.iter().map(|line| {
                    div()
                        .w_full()
                        .whitespace_normal()
                        .when(line.changed, |row| row.bg(p.selected.opacity(0.24)))
                        .child(line.text.clone())
                })),
        )
        .into_any_element()
}
impl Render for InventoryView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut view = v_flex()
            .id("sync-conflict-inventory")
            .debug_selector(|| "sync-conflict-inventory".into())
            .w_full()
            .min_h_0()
            .gap_2()
            .child(self.root.display().to_string())
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("sync-conflict-refresh")
                            .label("Refresh conflicts")
                            .small()
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    )
                    .child(
                        Button::new("sync-conflict-recover")
                            .label("Recover interrupted resolutions")
                            .small()
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, _, cx| this.recover(cx))),
                    ),
            )
            .child(self.message.clone());
        if self.pending > 0 {
            view = view.child(div().id("sync-conflict-pending").debug_selector(|| "sync-conflict-pending".into()).child(format!("{} interrupted conflict decision(s) need recovery. Both originals are retained in Note history and Recover notes.", self.pending)));
        }
        if self.busy {
            view = view.child("Loading…");
        }
        if !self.complete {
            view = view.child("Inventory is incomplete. Refresh to retry; absence here does not mean there are no conflicts.");
        }
        if self.complete && self.copies.is_empty() && self.panel.is_none() && self.pending == 0 {
            view = view.child("No conflict copies found.");
        }
        if let Some(panel) = &self.panel {
            view = view
                .child(
                    h_flex()
                        .id("sync-conflict-comparison")
                        .debug_selector(|| "sync-conflict-comparison".into())
                        .gap_4()
                        .child(pane(&panel.comparison.this, "sync-conflict-this", cx))
                        .child(pane(&panel.comparison.that, "sync-conflict-that", cx)),
                )
                .child(panel.message.clone())
                .child(h_flex().gap_2().children(ACTIONS.iter().enumerate().map(
                    |(index, (choice, label))| {
                        let choice = *choice;
                        Button::new(("sync-conflict-choice", index))
                            .debug_selector(move || format!("sync-conflict-choice-{index}").into())
                            .label(*label)
                            .disabled(self.busy || !matches!(panel.state, State::Ready))
                            .on_click(cx.listener(move |this, _, _, cx| this.apply(choice, cx)))
                    },
                )));
            if let Some(receipt) = &panel.recovery {
                view = view.child(format!(
                    "Both originals are in Note history and Recover notes.{}",
                    if matches!(panel.state, State::NeedsAttention) {
                        format!(" Recovery record: {}", receipt.recovery_record.display())
                    } else {
                        String::new()
                    }
                ));
                if let Some(kept) = &receipt.kept_both {
                    view = view.child(format!("Other version kept as {}", kept.display()));
                }
            }
        } else {
            let pages = self.copies.len().div_ceil(50);
            view = view.child(
                v_flex()
                    .id("sync-conflict-copies")
                    .max_h(px(300.))
                    .overflow_y_scroll()
                    .children(
                        self.copies
                            .iter()
                            .enumerate()
                            .skip(self.page * 50)
                            .take(50)
                            .map(|(index, copy)| {
                                let copy = copy.clone();
                                Button::new(("sync-conflict-copy", index))
                                    .label(copy.display().to_string())
                                    .disabled(self.busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.preview(copy.clone(), cx)
                                    }))
                            }),
                    ),
            );
            if pages > 1 {
                view = view.child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("sync-conflict-prev")
                                .label("Previous")
                                .disabled(self.page == 0)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.page -= 1;
                                    cx.notify();
                                })),
                        )
                        .child(format!("{} / {pages}", self.page + 1))
                        .child(
                            Button::new("sync-conflict-next")
                                .label("Next")
                                .disabled(self.page + 1 >= pages)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.page += 1;
                                    cx.notify();
                                })),
                        ),
                );
            }
        }
        view
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui_component::Root;
    use okilum_sync_controller::conflicts::ApplyOutcome;

    #[gpui::test]
    fn issue2044_mounted_inventory_comparison_and_buttons_resolve_both_kinds(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::test_init);
        for marker in ["sync", "okilum"] {
            for index in 0..3 {
                let directory = tempfile::tempdir().unwrap();
                let root = directory.path().join("vault");
                let state = directory.path().join("state");
                std::fs::create_dir(&root).unwrap();
                std::fs::create_dir(&state).unwrap();
                let copy =
                    PathBuf::from(format!("note.{marker}-conflict-20261104-123456-ABC2345.md"));
                std::fs::write(root.join("note.md"), b"same\r\nthis\r\n").unwrap();
                std::fs::write(root.join(&copy), b"same\r\nthat\r\n").unwrap();
                let (root_view, visual) = cx.add_window_view(|window, cx| {
                    let view =
                        cx.new(|cx| InventoryView::new(root.clone(), state.clone(), None, cx));
                    Root::new(view, window, cx)
                });
                visual.simulate_resize(size(px(1300.), px(900.)));
                visual.run_until_parked();
                let view = root_view.read_with(visual, |root: &Root, _| {
                    root.view().clone().downcast::<InventoryView>().unwrap()
                });
                view.read_with(visual, |v, _| {
                    assert!(v.complete);
                    assert_eq!(v.copies, [copy.clone()]);
                });
                view.update(visual, |v, cx| v.preview(copy.clone(), cx));
                visual.run_until_parked();
                assert!(
                    visual.debug_bounds("sync-conflict-comparison").is_some(),
                    "two panes are mounted"
                );
                view.read_with(visual, |v, _| {
                    let p = v.panel.as_ref().unwrap();
                    assert!(p.comparison.this.lines[1].changed);
                    assert!(p.comparison.that.lines[1].changed);
                    assert!(!p.comparison.this.lines[0].changed);
                    assert!(p
                        .comparison
                        .that
                        .provenance
                        .contains(if marker == "okilum" {
                            "random"
                        } else {
                            "device prefix"
                        }));
                });
                let bounds = visual
                    .debug_bounds(
                        [
                            "sync-conflict-choice-0",
                            "sync-conflict-choice-1",
                            "sync-conflict-choice-2",
                        ][index],
                    )
                    .expect("choice button mounted");
                visual.simulate_click(bounds.center(), Modifiers::default());
                visual.run_until_parked();
                view.read_with(visual, |v, _| {
                    assert!(
                        matches!(v.panel.as_ref().unwrap().state, State::Resolved),
                        "{}",
                        v.panel.as_ref().unwrap().message
                    )
                });
                assert_eq!(
                    std::fs::read(root.join("note.md")).unwrap(),
                    if index == 1 {
                        b"same\r\nthat\r\n"
                    } else {
                        b"same\r\nthis\r\n"
                    }
                );
                assert!(!root.join(&copy).exists());
                let mut listing = okilum_core::source_history::Listing::default();
                append_history(&state, &root, &mut listing).unwrap();
                assert_eq!(listing.versions.len(), 2);
                assert_eq!(listing.versions[0].bytes(), b"same\r\nthis\r\n");
                assert_eq!(listing.versions[1].bytes(), b"same\r\nthat\r\n");
                assert!(
                    listing.versions.iter().all(|version| version.raw.is_none()),
                    "valid UTF-8 history must not claim a lossy preview"
                );
                assert!(listing
                    .versions
                    .iter()
                    .all(|v| v.note == root.join("note.md")));
                visual.update(|window, _| window.remove_window());
            }
        }
    }
    #[gpui::test]
    fn issue2044_symlink_inventory_blocks_a_dirty_canonical_reader(cx: &mut TestAppContext) {
        cx.update(crate::test_init);
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("vault");
        let alias = directory.path().join("vault-alias");
        let state = directory.path().join("state");
        std::fs::create_dir(&root).unwrap();
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        let copy = PathBuf::from("note.sync-conflict-20261104-123456-ABC2345.md");
        std::fs::write(root.join("note.md"), b"this").unwrap();
        std::fs::write(root.join(&copy), b"that").unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    crate::Opts {
                        vault: Some(root.clone()),
                        note: Some("note.md".into()),
                        session_directory: Some(state.clone()),
                        index_dir: Some(directory.path().join("index")),
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
        let inventory = visual.update(|_, cx| {
            cx.new(|cx| {
                InventoryView::new(alias.clone(), state.clone(), Some(reader.downgrade()), cx)
            })
        });
        visual.run_until_parked();
        inventory.read_with(visual, |view, _| {
            assert_eq!(view.root, root.canonicalize().unwrap());
            assert_eq!(view.copies, [copy.clone()]);
        });
        inventory.update(visual, |view, cx| view.preview(copy.clone(), cx));
        visual.run_until_parked();
        reader.update_in(visual, |reader, window, cx| {
            reader.toggle_source(window, cx);
            reader.source_input().unwrap().update(cx, |input, cx| {
                input.replace_text_in_range(Some(0..0), "unsaved edit", window, cx);
            });
            assert!(reader.source_is_dirty(cx), "positive dirty control");
        });
        inventory.update(visual, |view, cx| {
            assert!(
                view.dirty(cx),
                "symlink inventory must see the canonical editor"
            );
            view.apply(Choice::KeepThat, cx);
            let panel = view.panel.as_ref().unwrap();
            assert!(matches!(panel.state, State::Ready));
            assert!(panel.message.contains("unsaved draft"));
        });
        assert_eq!(std::fs::read(root.join("note.md")).unwrap(), b"this");
        assert_eq!(std::fs::read(root.join(copy)).unwrap(), b"that");
    }

    #[gpui::test]
    fn issue2044_interrupted_inventory_recovery_reveals_recreated_copy(cx: &mut TestAppContext) {
        cx.update(crate::test_init);
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("vault");
        let state = directory.path().join("state");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&state).unwrap();
        let copy = PathBuf::from("note.okilum-conflict-20261104-123456-ABC2345.md");
        std::fs::write(root.join("note.md"), b"this").unwrap();
        std::fs::write(root.join(&copy), b"that").unwrap();
        let recovery = state.join("sync-conflicts");
        conflicts::prepare_recovery(&recovery).unwrap();
        let preview = conflicts::preview(&root, &copy).unwrap();
        let ApplyOutcome::Applied(receipt) =
            conflicts::apply(&preview, Choice::KeepBoth, &recovery).unwrap()
        else {
            panic!("fixture must apply");
        };
        std::fs::remove_file(receipt.recovery_record.with_extension("complete")).unwrap();
        std::fs::write(root.join("note.md"), b"later writer").unwrap();
        let (root_view, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| InventoryView::new(root.clone(), state.clone(), None, cx));
            Root::new(view, window, cx)
        });
        visual.run_until_parked();
        let view = root_view.read_with(visual, |root, _| {
            root.view().clone().downcast::<InventoryView>().unwrap()
        });
        view.read_with(visual, |v, _| {
            assert_eq!(v.pending, 1);
            assert!(v.copies.is_empty());
        });
        view.update(visual, |v, cx| v.recover(cx));
        visual.run_until_parked();
        view.read_with(visual, |v, _| {
            assert_eq!(v.pending, 0);
            assert_eq!(v.copies, [copy.clone()]);
            assert!(v.complete);
        });
        assert_eq!(
            std::fs::read(root.join("note.md")).unwrap(),
            b"later writer"
        );
        assert_eq!(std::fs::read(root.join(copy)).unwrap(), b"that");
        assert!(receipt
            .recovery_record
            .with_extension("recovered")
            .is_file());
    }
}
