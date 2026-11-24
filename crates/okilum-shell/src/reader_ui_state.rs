//! Reader side of the UI state (#3233, epic #3241): the per-window `Session`,
//! restoring and recording a Reader's layout, and the registry of open Readers
//! that quit and update restart record. Persistence lives in
//! `okilum_shell_kit::reader_ui_state`, re-exported here under the same path.
use super::*;
pub(crate) use okilum_shell_kit::reader_ui_state::*;
// Names the root also defines: the kit's store functions win here, as they
// did when this module defined them.
#[cfg(test)]
use okilum_shell_kit::reader_ui_state::Position;
pub(crate) use okilum_shell_kit::reader_ui_state::{set_appearance, set_theme};
#[cfg(test)]
use std::collections::BTreeSet;

/// Open Readers and the vault each active window shows (#1338).
#[derive(Default)]
struct Readers {
    readers: Vec<WeakEntity<Reader>>,
    active_roots: std::collections::HashMap<gpui::EntityId, PathBuf>,
}
impl Global for Readers {}

/// Installs the kit store, then the Reader registry and its hook: quit and
/// `prepare_for_restart` record every open Reader first.
pub(crate) fn install(directory: &Path, cx: &mut App) {
    okilum_shell_kit::reader_ui_state::install(directory, cx);
    cx.set_global(Readers::default());
    set_record_open_windows(record_open_readers, cx);
}

fn record_open_readers(cx: &mut App) {
    let readers = cx
        .try_global::<Readers>()
        .map(|r| r.readers.clone())
        .unwrap_or_default();
    for reader in readers {
        let _ = reader.update(cx, |reader, cx| {
            reader.record_ui_state(reader.ui_state.active, cx)
        });
    }
}

#[derive(Default)]
pub(crate) struct Session {
    root: PathBuf,
    workspace: Option<(PathBuf, u64)>,
    pub(super) restoring_workspace: bool,
    last: Option<Layout>,
    active: bool,
    tree: Option<Layout>,
    source: Option<[f32; 2]>,
    pub(super) live_preview: bool,
    /// This window keeps its chosen editing mode across Markdown file choices.
    /// Session restoration still uses the existing saved source/LP fields.
    pub(super) source_mode: bool,
    /// The note on which the person explicitly chose Read: the Settings
    /// default that opens notes in Edit (#2817) does not reopen it until
    /// another note is opened.
    pub(super) read_chosen: Option<String>,
    /// The open editor came from that Settings default, not from a choice:
    /// it owns nothing until the first edit and is never saved as the
    /// session's editing mode (#2817).
    pub(super) auto_edit: bool,
    /// `toggle_source` is opening an automatic editor.
    pub(super) opening_by_default: bool,
    /// The automatic editor was refused for a reason the person should see
    /// (another window edits the note, or it has an unsaved draft).
    pub(super) default_edit_notice: bool,
    /// The window's first note was published before its saved state was
    /// restored; the Settings default still applies to it (#2817).
    default_edit_pending: bool,
    source_position_pending: bool,
    source_reader_position: Option<ListOffset>,
    pub(crate) source_highlight_pending: bool,
    reader_position_pending: bool,
    pub(crate) ready: bool,
    pub(crate) interacted: bool,
}

impl Reader {
    /// Called once for a newly published canonical root, not for reconciliation.
    pub(crate) fn restore_ui_state(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ui_state.root == self.vault_root || !installed(cx) {
            return;
        }
        let weak = cx.entity().downgrade();
        let readers = &mut cx.default_global::<Readers>().readers;
        readers
            .retain(|reader| reader.upgrade().is_some() && reader.entity_id() != weak.entity_id());
        readers.push(weak);
        let interacted = self.ui_state.interacted;
        self.ui_state = Session {
            root: self.vault_root.clone(),
            ready: true,
            default_edit_pending: true,
            ..Default::default()
        };
        let Some((saved, known)) = layout(&self.vault_root, cx) else {
            return;
        };
        if !interacted {
            self.panels = saved.panels.clone();
            self.panel_widths
                .set(reader_layout::Panel::Notes, saved.widths.notes);
            self.panel_widths
                .set(reader_layout::Panel::Backlinks, saved.widths.backlinks);
        }
        self.project_navigation.layout = saved.project_layout.clone();
        // Reject legacy asynchronous width publication after this newer restore.
        self.panel_widths_revision = self.panel_widths_revision.wrapping_add(1);
        self.sidebar.collapsed = saved.collapsed.clone();
        self.sidebar.properties_collapsed = saved.properties_collapsed;
        self.sidebar.show_hidden = saved.show_hidden;
        self.recent_expanded = saved.recent_expanded;
        self.pane.properties_open = saved.properties_open;
        self.pane.show_hidden_properties = saved.hidden_properties;
        self.ui_state.tree = Some(saved.clone());
        self.ui_state.live_preview = saved.live_preview;
        if saved.source {
            self.ui_state.source_reader_position =
                (saved.note == self.pane.current_rel).then(|| saved.position.list());
            self.ui_state.source = Some(if saved.note == self.pane.current_rel {
                saved.source_scroll
            } else {
                [0.; 2]
            });
        }
        let explicit = self.loading.as_ref().is_some_and(|load| {
            load.opts.note.is_some()
                || load
                    .opts
                    .open_path
                    .as_ref()
                    .is_some_and(|path| path.extension().is_some())
        });
        let replay = self
            .loading
            .as_ref()
            .is_some_and(|load| load.opts.restore_workspace);
        if known && (!explicit || replay) && !interacted && self.pane.queued_open_note.is_none() {
            self.ui_state.workspace = saved
                .project
                .as_ref()
                .filter(|path| {
                    path.components()
                        .all(|part| matches!(part, std::path::Component::Normal(_)))
                })
                .map(|path| {
                    (
                        self.vault_root.join(path),
                        self.project_navigation.generation,
                    )
                });
        }
        if known && saved.note.is_empty() && !explicit {
            self.show_empty_vault(window, cx);
        }
        if known && saved.note == self.pane.current_rel {
            let visits: Vec<_> = saved
                .history
                .iter()
                .filter(|visit| valid_note(&visit.note))
                .cloned()
                .collect();
            if visits.len() == saved.history.len()
                && visits
                    .get(saved.history_index)
                    .is_some_and(|visit| visit.note == self.pane.current_rel)
            {
                self.pane.navigation.history =
                    visits.iter().map(|visit| visit.note.clone()).collect();
                self.pane.navigation.history_positions =
                    visits.iter().map(|visit| visit.position.list()).collect();
                self.pane.navigation.history_ix = saved.history_index;
            }
            if !saved.note.is_empty() && !saved.source {
                self.cancel_pending_landing();
                let positioned = self.pane.content.update(cx, |content, cx| {
                    content.scroll_to_prepared_position(saved.position.list(), cx)
                });
                if !positioned {
                    self.ui_state.reader_position_pending = true;
                    self.scroll_to_position(saved.position.list(), cx);
                }
            }
        }
        cx.notify();
    }

    /// Cancel before dispatching a newer user intent, not after its async I/O.
    /// Advancing the project generation makes the old validation callback inert.
    pub(crate) fn cancel_workspace_restore(&mut self) {
        if self.ui_state.workspace.take().is_some() || self.ui_state.restoring_workspace {
            self.ui_state.restoring_workspace = false;
            self.project_navigation.generation = self.project_navigation.generation.wrapping_add(1);
        }
    }

    pub(crate) fn restore_workspace_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ui_state.tree.is_some() || self.restoring_source() {
            return;
        }
        if let Some((folder, generation)) = self.ui_state.workspace.take() {
            if generation == self.project_navigation.generation
                && reader_features::enabled(reader_features::Feature::Agents, cx)
            {
                self.ui_state.restoring_workspace = true;
                self.enter_project_folder(folder, window, cx);
            }
        }
    }

    pub(crate) fn restore_ui_tree(&mut self) {
        // Keep expanded unknown folders until complete inventory is published.
        if !self.vault.inventory_scanned {
            return;
        }
        let Some(saved) = self.ui_state.tree.take() else {
            return;
        };
        self.tree.restore_expanded(saved.folders, saved.tree_cursor);
        self.tree_revealed = self.selected_file().to_owned();
        self.inbox_ready_root = Some(self.vault_root.clone());
        let offset = if saved.tree_scroll.is_finite() {
            saved.tree_scroll.min(0.)
        } else {
            0.
        };
        let mut scroll = self.tree_scroll.0.borrow_mut();
        // sync_tree may have queued a reveal before the saved layout arrived.
        // That request runs during layout and would overwrite this offset.
        scroll.deferred_scroll_to_item = None;
        scroll.base_handle.set_offset(point(px(0.), px(offset)));
    }

    pub(crate) fn restore_ui_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The selected document is usable before background search validation
        // finishes. The invisible Markdown preview need not finish laying out.
        if self
            .loading
            .as_ref()
            .is_some_and(|load| load.active && !load.published)
        {
            return;
        }
        // A saved editor restores itself below; otherwise the Settings
        // default may open the window's first note in Edit (#2817).
        if std::mem::take(&mut self.ui_state.default_edit_pending) && self.ui_state.source.is_none()
        {
            self.open_note_by_default(false, window, cx);
        }
        let Some(offset) = self.ui_state.source.take() else {
            return;
        };
        if self.pane.editing.is_none()
            && !self.pane.current_rel.is_empty()
            && self.pane.file_preview.is_none()
        {
            self.cancel_pending_landing();
            self.toggle_source(window, cx);
            if self.pane.editing.is_none() {
                if let Some(position) = self.ui_state.source_reader_position.take() {
                    // If draft recovery cannot open, preserve the preview landing
                    // as well as the existing error notice.
                    self.scroll_to_position(position, cx);
                }
            }
            self.ui_state.source_position_pending = self.pane.editing.is_some();
            if self.pane.editing.is_some() {
                self.ui_state.source_highlight_pending = self.source_highlighting_pending(cx);
            }
            self.restore_source_position(offset, window, cx);
            self.land_restored_source(window, cx);
            let reader = cx.entity().downgrade();
            window.on_next_frame(move |_, cx| {
                let _ = reader.update(cx, |reader, cx| {
                    reader.ui_state.source_position_pending = false;
                    cx.notify();
                });
            });
        }
    }

    /// A window about to restore its editor opens it for a link instead: at
    /// the top, not at the last session's scroll (#1049). False when the
    /// window is not restoring an editor.
    pub(crate) fn retarget_restored_editor(&mut self) -> bool {
        if self.ui_state.source.is_none() {
            return false;
        }
        self.ui_state.source = Some([0.; 2]);
        self.ui_state.source_reader_position = None;
        true
    }

    pub(crate) fn restoring_source(&self) -> bool {
        self.ui_state.source.is_some()
            || self.ui_state.source_position_pending
            || (self.pane.editing.is_some() && self.ui_state.source_highlight_pending)
    }

    pub(crate) fn restoring_reader(&self) -> bool {
        self.ui_state.reader_position_pending && self.pane.navigation.pending_landing.is_some()
    }

    pub(crate) fn record_ui_state(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.pane.navigation.pending_landing.is_none() {
            self.ui_state.reader_position_pending = false;
        }
        if self.ui_state.workspace.is_some()
            || self.ui_state.restoring_workspace
            || !self.ui_state.ready
            || self.ui_state.root != self.vault_root
        {
            return;
        }
        // A background sibling must not replace the active window's viewport
        // when both show the same vault (including during quit).
        if !active && {
            let me = cx.entity_id();
            cx.try_global::<Readers>().is_some_and(|store| {
                store.readers.iter().any(|other| {
                    other.entity_id() != me
                        && other.upgrade().is_some()
                        && store.active_roots.get(&other.entity_id()) == Some(&self.vault_root)
                })
            })
        } {
            self.set_ui_active(false, cx);
            return;
        }
        if self.pane.editing.is_none() && self.ui_state.source.is_none() {
            self.ui_state.source_reader_position = None;
        }
        let pending = self.ui_state.tree.as_ref();
        let position = self
            .ui_state
            .source_reader_position
            .or(self.pane.navigation.pending_landing)
            .unwrap_or_else(|| self.pane.content.read(cx).list_state().logical_scroll_top());
        let history = self
            .pane
            .navigation
            .history
            .iter()
            .enumerate()
            .map(|(index, note)| Visit {
                note: note.clone(),
                position: if index == self.pane.navigation.history_ix {
                    position.into()
                } else {
                    self.pane
                        .navigation
                        .history_positions
                        .get(index)
                        .copied()
                        .unwrap_or(ListOffset {
                            item_ix: 0,
                            offset_in_item: px(0.),
                        })
                        .into()
                },
            })
            .collect();
        let offset = self
            .source_scroll_offset(cx)
            .map(|p| [p.x.into(), p.y.into()])
            .or(self.ui_state.source)
            .unwrap_or([0.; 2]);
        let world = self.project_navigation.saved.as_ref();
        let tree = world.map_or(&self.tree, |saved| &saved.tree);
        let saved = Layout {
            project: self.project_navigation.active.as_ref().and_then(|project| {
                project
                    .folder
                    .strip_prefix(&self.vault_root)
                    .ok()
                    .map(Path::to_path_buf)
            }),
            panels: world.map_or_else(
                || self.panels.preferences(),
                |saved| saved.panels.preferences(),
            ),
            widths: world.map_or_else(|| self.panel_widths.clone(), |saved| saved.widths.clone()),
            project_layout: if self.project_navigation.active.is_some() {
                Some(reader_layout::ViewLayout::capture(
                    &self.panels,
                    &self.panel_widths,
                ))
            } else {
                self.project_navigation.layout.clone()
            },
            collapsed: self.sidebar.collapsed.clone(),
            properties_collapsed: self.sidebar.properties_collapsed,
            show_hidden: self.sidebar.show_hidden,
            folders: pending
                .map(|state| state.folders.clone())
                .unwrap_or_else(|| tree.expanded_paths().clone()),
            tree_cursor: pending
                .and_then(|state| state.tree_cursor.clone())
                .or_else(|| tree.cursor.clone()),
            tree_scroll: pending.map(|state| state.tree_scroll).unwrap_or_else(|| {
                world.map_or_else(
                    || self.tree_scroll.0.borrow().base_handle.offset().y.into(),
                    |saved| saved.tree_offset.y.into(),
                )
            }),
            recent_expanded: self.recent_expanded,
            properties_open: self.pane.properties_open,
            hidden_properties: self.pane.show_hidden_properties,
            note: self.selected_file().to_owned(),
            history,
            history_index: self.pane.navigation.history_ix,
            position: position.into(),
            // An automatic Edit is the Settings default, not this session's
            // choice: relaunch applies the setting again, never as a choice
            // that would carry to text, data or code files (#2817).
            source: (self.pane.editing.is_some() && !self.ui_state.auto_edit)
                || self.ui_state.source.is_some(),
            live_preview: if self.pane.editing.is_some() {
                self.source_live_preview()
            } else {
                self.ui_state.live_preview
            },
            source_scroll: offset,
        };
        if self.ui_state.last.as_ref() != Some(&saved) || (active && !self.ui_state.active) {
            self.ui_state.last = Some(saved.clone());
            record(&self.vault_root, saved, active, cx);
        }
        self.set_ui_active(active, cx);
    }

    fn set_ui_active(&mut self, active: bool, cx: &mut Context<Self>) {
        self.ui_state.active = active;
        let id = cx.entity_id();
        let root = self.vault_root.clone();
        let roots = &mut cx.default_global::<Readers>().active_roots;
        if active {
            roots.insert(id, root);
        } else {
            roots.remove(&id);
        }
    }
}

impl Session {
    pub(crate) fn was_active(&self) -> bool {
        self.active
    }
}
pub(crate) fn capture_next_frame(reader: WeakEntity<Reader>, window: &Window) {
    window.on_next_frame(move |window, cx| {
        let _ = reader.update(cx, |reader, cx| {
            reader.record_ui_state(window.is_window_active(), cx)
        });
    });
}
pub(crate) fn capture_scroll(reader: WeakEntity<Reader>) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |_, _, window, _| {
            let reader = reader.clone();
            window.on_mouse_event(move |_: &ScrollWheelEvent, phase, window, _| {
                if phase.capture() {
                    capture_next_frame(reader.clone(), window);
                }
            });
        },
    )
    .absolute()
    .size_full()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui_component::WindowExt;

    #[gpui::test]
    fn restoring_closed_note_keeps_the_vault_window_open(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("note.md"), "# Note").unwrap();
        let directory = fixture.path().join("state");
        reader_history::ReadingHistory::record_usable_document(&directory, &root, "").unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            install(&directory, cx);
            record(&root, Layout::default(), true, cx);
            flush(cx);
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root),
                        session_directory: Some(directory),
                        index_dir: Some(fixture.path().join("index")),
                        panel_settings_override: Some(fixture.path().join("legacy-widths.json")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        visual.run_until_parked();
        reader.unwrap().read_with(visual, |reader, cx| {
            assert!(
                reader.ui_state.ready,
                "positive control: vault state was restored"
            );
            assert!(reader.pane.current_rel.is_empty());
            assert_eq!(
                cx.windows().len(),
                1,
                "empty selection is not a close-window action"
            );
        });
    }

    #[gpui::test]
    fn restored_reader_keeps_panels_sections_folders_history_and_scroll(cx: &mut TestAppContext) {
        restored_document_state(cx, false, false);
    }

    #[gpui::test]
    fn restored_source_hides_preview_until_source_viewport_is_ready(cx: &mut TestAppContext) {
        restored_document_state(cx, true, false);
    }

    #[gpui::test]
    fn restores_live_preview_and_recovered_draft(cx: &mut TestAppContext) {
        restored_document_state(cx, true, true);
    }

    fn restored_document_state(cx: &mut TestAppContext, source: bool, live_preview: bool) {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        std::fs::create_dir_all(root.join("Folder")).unwrap();
        std::fs::write(
            root.join("Folder/note.md"),
            (0..40)
                .map(|i| format!("## Heading {i}\n\nParagraph {i}.\n\n"))
                .collect::<String>(),
        )
        .unwrap();
        std::fs::write(root.join("other.md"), "# Other").unwrap();
        for i in 0..100 {
            std::fs::write(root.join(format!("tree-{i:03}.md")), "# Tree entry").unwrap();
        }
        // Legacy history disagrees with the newer UI snapshot (e.g. an early
        // quit while its independent background write was still pending).
        let directory = fixture.path().join("state");
        reader_history::ReadingHistory::record_usable_document(&directory, &root, "other.md")
            .unwrap();
        let saved = Layout {
            panels: reader_layout::Panels {
                notes: true,
                backlinks: false,
                active: reader_layout::Panel::Notes,
                ..Default::default()
            },
            widths: reader_layout::Widths {
                notes: 355.,
                backlinks: 330.,
            },
            collapsed: BTreeSet::from([
                reader_sidebar::Section::Recent,
                reader_sidebar::Section::Pinned,
            ]),
            folders: BTreeSet::new(),
            tree_cursor: Some("Folder".into()),
            tree_scroll: -840.,
            source,
            live_preview,
            source_scroll: [0., -400.],
            note: "Folder/note.md".into(),
            position: Position {
                item: 14,
                offset: 0.,
            },
            history: vec![
                Visit {
                    note: "other.md".into(),
                    position: Position::default(),
                },
                Visit {
                    note: "Folder/note.md".into(),
                    position: Position {
                        item: 14,
                        offset: 0.,
                    },
                },
            ],
            history_index: 1,
            ..Default::default()
        };
        cx.update(|cx| {
            crate::test_init(cx);
            install(&directory, cx);
            record(&root, saved.clone(), true, cx);
            flush(cx);
            install(&directory, cx);
        });
        if live_preview {
            let path = root.join("Folder/note.md");
            let mut draft =
                okilum_core::file_editor::FileEditor::open(&path, &directory.join("editor-drafts"))
                    .unwrap();
            let text = format!("{}\nUnsaved draft sentinel\n", draft.text());
            draft.set_text(text).unwrap();
            cx.update(|cx| cx.set_global(reader_recovery::RecoveryStartup(true)));
        }
        let (release_search, hold_search) = async_channel::bounded(1);
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: None,
                        preparation_hold: Some(hold_search),
                        session_directory: Some(directory.clone()),
                        index_dir: Some(fixture.path().join("index")),
                        panel_settings_override: Some(fixture.path().join("legacy-widths.json")),
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
        if !source {
            reader.read_with(visual, |reader, cx| {
                assert!(
                    reader.ui_state.ready,
                    "positive control: publication completed"
                );
                assert_eq!(
                    reader
                        .pane
                        .content
                        .read(cx)
                        .list_state()
                        .logical_scroll_top()
                        .item_ix,
                    14,
                    "saved position must be installed before any landing timer"
                );
            });
        }
        visual.executor().advance_clock(Duration::from_millis(100));
        visual.run_until_parked();
        {
            // Search preparation is deliberately held indefinitely. The
            // published source document must still reach its saved viewport.
            for _ in 0..5 {
                visual.update(|window, cx| {
                    window.simulate_next_frame(cx);
                    window.draw(cx).clear(cx);
                });
                visual.executor().advance_clock(Duration::from_millis(20));
                visual.run_until_parked();
            }
            reader.read_with(visual, |reader, cx| {
                let load = reader.loading.as_ref().unwrap();
                assert!(load.active && load.published, "search remains unfinished");
                if source {
                    assert!(
                        reader.pane.editing.is_some(),
                        "source must not wait for search"
                    );
                    assert!(
                        !reader.restoring_source(),
                        "source viewport must be visible"
                    );
                    assert_eq!(reader.source_live_preview(), live_preview);
                    if !live_preview {
                        assert_eq!(reader.source_scroll_offset(cx).unwrap().y, px(-400.));
                    }
                } else {
                    assert_eq!(
                        reader
                            .pane
                            .content
                            .read(cx)
                            .list_state()
                            .logical_scroll_top()
                            .item_ix,
                        14
                    );
                    assert!(
                        reader.pane.navigation.pending_landing.is_none(),
                        "first reader frame is positioned without a timer"
                    );
                }
                assert_eq!(
                    layout(&root, cx).unwrap().0.position.item,
                    14,
                    "unpainted preview position must survive persisted source restoration"
                );
            });
            {
                assert_eq!(
                    visual
                        .debug_bounds("document-header-viewport")
                        .unwrap()
                        .size
                        .height,
                    px(48.),
                    "restoration must keep breadcrumbs visible at a mid-note position"
                );
            }
            release_search.try_send(()).unwrap();
            visual.run_until_parked();
        }
        reader.update(visual, |reader, _| {
            // Native startup can publish state between sync_tree's reveal and
            // the list's first layout. Exercise that ordering explicitly.
            assert!(reader.vault.inventory_scanned);
            reader.ui_state.tree = Some(saved.clone());
            reader.tree_scroll.scroll_to_item(0, ScrollStrategy::Center);
            reader.restore_ui_tree();
        });
        for _ in 0..5 {
            reader.read_with(visual, |reader, _| {
                if source && reader.ui_state.ready && reader.pane.editing.is_none() {
                    assert!(reader.restoring_source(), "preview must remain hidden");
                }
            });
            visual.update(|window, cx| {
                window.simulate_next_frame(cx);
                window.draw(cx).clear(cx);
            });
            visual.executor().advance_clock(Duration::from_millis(20));
            visual.run_until_parked();
        }
        reader.read_with(visual, |reader, cx| {
            assert!(
                reader.document_ready(),
                "positive control: actual document loaded"
            );
            assert_eq!(reader.panels, saved.panels);
            assert_eq!(
                reader.panel_widths, saved.widths,
                "legacy async defaults cannot overwrite saved widths"
            );
            assert_eq!(reader.sidebar.collapsed, saved.collapsed);
            assert_eq!(reader.tree.expanded_paths(), &saved.folders);
            assert_eq!(
                reader.tree_scroll.0.borrow().base_handle.offset().y,
                px(saved.tree_scroll),
                "saved tree scroll must survive the initial selected-note reveal"
            );
            if source {
                assert!(
                    reader.pane.editing.is_some(),
                    "positive control: source mounted"
                );
                assert!(
                    !reader.restoring_source(),
                    "restored source must become visible"
                );
                assert_eq!(reader.source_live_preview(), live_preview);
                if live_preview {
                    let input = reader.pane.editing.as_ref().unwrap().test_input();
                    assert!(input.read(cx).value().contains("Unsaved draft sentinel"));
                    assert!(!std::fs::read_to_string(root.join("Folder/note.md"))
                        .unwrap()
                        .contains("Unsaved draft sentinel"));
                } else {
                    assert_eq!(reader.source_scroll_offset(cx).unwrap().y, px(-400.));
                }
            }
            assert_eq!(
                reader.pane.navigation.history,
                ["other.md", "Folder/note.md"]
            );
            assert_eq!(reader.pane.navigation.history_ix, 1);
            assert_eq!(
                if source {
                    reader.ui_state.source_reader_position.unwrap().item_ix
                } else {
                    reader
                        .pane
                        .content
                        .read(cx)
                        .list_state()
                        .logical_scroll_top()
                        .item_ix
                },
                14
            );
        });
        if live_preview {
            visual.update(|window, cx| {
                assert!(
                    !window.notifications(cx).is_empty(),
                    "positive control: restored draft announces itself"
                );
                // #930: the draft is already applied, so the offer to
                // restore it must not appear and push the notice away.
                assert_eq!(window.notifications(cx).len(), 1, "one recovery notice");
                window.draw(cx).clear(cx);
            });
            assert!(
                visual.debug_bounds("restore-unsaved-edits").is_none(),
                "no «Restore unsaved edits» for an already restored draft"
            );
            // #930: «Unsaved changes restored» stays readable for most of
            // its 4 s lifetime; nothing pushes it away early.
            visual.executor().advance_clock(Duration::from_secs(3));
            visual.run_until_parked();
            visual.update(|window, cx| {
                assert_eq!(
                    window.notifications(cx).len(),
                    1,
                    "the restored notice is still shown after 3 s"
                );
                window.draw(cx).clear(cx);
            });
            assert!(
                visual.debug_bounds("restore-unsaved-edits").is_none(),
                "and it is not the offer to restore"
            );
            visual.executor().advance_clock(Duration::from_secs(2));
            visual.run_until_parked();
            visual.update(|window, cx| {
                assert!(
                    window.notifications(cx).is_empty(),
                    "successful recovery notice must expire even while edits remain unsaved"
                )
            });
            reader.read_with(visual, |reader, cx| {
                assert!(reader.link_notice.is_none());
                assert!(reader.source_is_dirty(cx));
            });
            reader.update_in(visual, |reader, _, cx| {
                reader.open_source_find(cx);
                reader.record_ui_state(true, cx);
                assert!(
                    layout(&root, cx).unwrap().0.live_preview,
                    "temporary Source during Find must not overwrite the saved choice"
                );
            });
        }
        {
            for _ in 0..2 {
                let before = reader.read_with(visual, |reader, cx| {
                    let position = reader
                        .pane
                        .content
                        .read(cx)
                        .list_state()
                        .logical_scroll_top();
                    (
                        reader.source_scroll_offset(cx),
                        position.item_ix,
                        position.offset_in_item,
                    )
                });
                visual.update(|window, cx| window.draw(cx).clear(cx));
                let body = visual.debug_bounds("reader-document").unwrap();
                visual.simulate_event(ScrollWheelEvent {
                    position: point(body.center().x, body.top() + px(130.)),
                    delta: ScrollDelta::Pixels(point(px(0.), px(-24.))),
                    ..Default::default()
                });
                visual.run_until_parked();
                let after = reader.read_with(visual, |reader, cx| {
                    let position = reader
                        .pane
                        .content
                        .read(cx)
                        .list_state()
                        .logical_scroll_top();
                    (
                        reader.source_scroll_offset(cx),
                        position.item_ix,
                        position.offset_in_item,
                    )
                });
                assert_ne!(before, after, "positive control: restored content scrolls");
                visual.update(|window, cx| window.draw(cx).clear(cx));
                assert_eq!(
                    visual
                        .debug_bounds("document-header-viewport")
                        .unwrap()
                        .size
                        .height,
                    px(48.),
                    "restored header stays pinned during explicit scrolling"
                );
            }
        }
    }
}
