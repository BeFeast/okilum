//! Actions and empty selection for the document, separate from app controls.
use super::*;
use crate::platform::labels::Os;

impl Reader {
    fn document_at_top(&self, cx: &App) -> bool {
        if let Some(offset) = self.source_scroll_offset(cx) {
            return offset.y >= px(-0.5);
        }
        let offset = self.content.read(cx).list_state().logical_scroll_top();
        offset.item_ix == 0 && offset.offset_in_item <= px(0.5)
    }

    pub(super) fn reader_top_inset(&self, cx: &App) -> Pixels {
        // Keep opening breathing room at the top without a stationary blank
        // band over scrolled text. The separate header always stays visible.
        if self.document_at_top(cx) {
            px(44.)
        } else {
            px(0.)
        }
    }

    pub(super) fn render_document_surface(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.active_timeline().is_some_and(|t| t.selected.is_some())
            || self.file_preview.is_some()
        {
            return self.render_main(window, cx);
        }
        v_flex()
            .size_full()
            .min_h_0()
            .relative()
            .child(
                div()
                    .debug_selector(|| "document-header-viewport".into())
                    .flex_none()
                    .overflow_hidden()
                    .h(px(48.))
                    .child(self.render_document_header(window, cx)),
            )
            .child(div().flex_1().min_h_0().child(self.render_main(window, cx)))
            .into_any_element()
    }

    pub(super) fn render_document_header(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
        let is_file = self.file_preview.is_some();
        #[cfg(any(unix, windows))]
        let editing = self.editing.is_some();
        #[cfg(any(unix, windows))]
        let reader = cx.entity().downgrade();
        let available = if self.body_bounds.size.width > px(0.) {
            self.body_bounds.size.width.as_f32()
        } else {
            window.viewport_size().width.as_f32()
        };
        let widths = self.panels.widths(&self.panel_widths, available);
        let document_width = available
            - if reader_layout::overlay(available) {
                0.
            } else {
                widths.notes + widths.backlinks
            };
        let title_width = toolbar_text_width(&self.selected_title(), FontWeight::MEDIUM, window);
        #[cfg(any(unix, windows))]
        let dirty = self.source_is_dirty(cx);
        #[cfg(not(any(unix, windows)))]
        let dirty = false;
        // Reserve padding, gaps, navigation, Read/Edit, menu and the dirty dot.
        // Optional controls yield before either the parent path or note title.
        let parent_width = self
            .selected_file()
            .rsplit_once('/')
            .map_or(0., |(dir, _)| {
                dir.split('/')
                    .map(|part| toolbar_text_width(part, FontWeight::NORMAL, window) + 24.)
                    .sum::<f32>()
            });
        let compact_parent = if parent_width > 0. { 32. } else { 0. };
        let fixed_width = 208. + if dirty { 32. } else { 0. };
        let mut spare = document_width - fixed_width - title_width - compact_parent;
        let show_presentation = self.editing.is_some() && spare >= 64.;
        if show_presentation {
            spare -= 64.;
        }
        let show_save = dirty && spare >= 32.;
        if show_save {
            spare -= 32.;
        }
        let show_find = spare >= 32.;
        if show_find {
            spare -= 32.;
        }
        #[cfg(any(unix, windows))]
        let labels_width = ["Read", "Edit"]
            .into_iter()
            .chain(if show_presentation {
                vec!["Live Preview", "Source"]
            } else {
                vec![]
            })
            .map(|label| toolbar_text_width(label, FontWeight::MEDIUM, window) + 12.)
            .sum::<f32>();
        #[cfg(any(unix, windows))]
        let labels = reader_ui_state::toolbar_labels(cx) && spare >= labels_width;
        #[cfg(any(unix, windows))]
        if labels {
            spare -= labels_width;
        }
        let collapse_parents = parent_width > compact_parent + spare.max(0.);
        let root = self.vault_root.clone();
        let rel = self.selected_file().to_owned();
        let mut row = h_flex()
            .id("document-header")
            .debug_selector(|| "document-header".into())
            .flex_none()
            .h(px(48.))
            .w_full()
            .min_w_0()
            .px_4()
            .gap_1()
            .child(
                reader_icon_button(
                    "reader-history-back",
                    IconName::ArrowLeft,
                    reader_shortcuts::hint("Back", &HistoryBack, cx),
                    cx,
                )
                .debug_selector(|| "reader-history-back".into())
                .disabled(self.navigation.history_ix == 0)
                .on_click(cx.listener(|this, _, window, cx| this.history_move(-1, window, cx))),
            )
            .child(
                reader_icon_button(
                    "reader-history-forward",
                    IconName::ArrowRight,
                    reader_shortcuts::hint("Forward", &HistoryForward, cx),
                    cx,
                )
                .debug_selector(|| "reader-history-forward".into())
                .disabled(self.navigation.history_ix + 1 >= self.navigation.history.len())
                .on_click(cx.listener(|this, _, window, cx| this.history_move(1, window, cx))),
            )
            .child(self.render_breadcrumbs(collapse_parents, cx));
        if rel.is_empty() {
            return row;
        }
        #[cfg(any(unix, windows))]
        if !is_file {
            use gpui_component::button::ButtonGroup;
            row = row.child(
                ButtonGroup::new("note-mode").children([
                    Button::new("reader-read")
                        .ghost()
                        .small()
                        .icon(IconName::BookOpen)
                        .h(px(28.))
                        .when(!labels, |b| b.w(px(28.)))
                        .when(labels, |button| button.label("Read"))
                        .selected(!editing)
                        .accessibility_label("Read")
                        .tooltip(reader_shortcuts::hint("Read", &ToggleSource, cx))
                        .debug_selector(|| "reader-read".into())
                        .on_click(cx.listener(|this, _, window, cx| {
                            if this.editing.is_some() {
                                this.toggle_source(window, cx);
                            }
                        })),
                    Button::new("reader-edit")
                        .ghost()
                        .small()
                        .icon(Icon::default().path("icons/pencil.svg"))
                        .h(px(28.))
                        .when(!labels, |b| b.w(px(28.)))
                        .when(labels, |button| button.label("Edit"))
                        .selected(editing)
                        .accessibility_label("Edit")
                        .tooltip(reader_shortcuts::hint("Edit", &ToggleSource, cx))
                        .debug_selector(|| "reader-edit".into())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.toggle_source(window, cx);
                        })),
                ]),
            );
            if editing {
                if show_presentation {
                    row = row.child(self.render_live_preview_control(labels, cx));
                }
                if self.source_is_dirty(cx) {
                    row = row.child(self.render_save_status(cx));
                    if show_save {
                        row = row.child(
                            reader_icon_button(
                                "source-save",
                                Icon::default().path("icons/save.svg"),
                                reader_shortcuts::hint("Save", &SaveSource, cx),
                                cx,
                            )
                            .debug_selector(|| "source-save".into())
                            .on_click(cx.listener(|this, _, _, cx| this.request_source_save(cx))),
                        );
                    }
                }
            }
        }
        if !is_file && show_find {
            row = row.child(
                reader_icon_button(
                    "note-find",
                    Icon::default().path("icons/text-search.svg"),
                    reader_shortcuts::hint("Find in note", &FindInNote, cx),
                    cx,
                )
                .on_click(cx.listener(|this, _, window, cx| this.open_find(window, cx))),
            );
        }
        if is_file {
            row = row.children(self.render_pdf_controls(cx));
            #[cfg(target_os = "macos")]
            let actions = vec![
                (
                    "file-quicklook",
                    IconName::Eye,
                    reader_shortcuts::hint("Quick Look", &QuickLookFile, cx),
                    reader_files::FileAction::QuickLook,
                ),
                (
                    "file-open",
                    IconName::ExternalLink,
                    SharedString::from("Open with default app"),
                    reader_files::FileAction::Open,
                ),
                (
                    "file-reveal",
                    IconName::FolderOpen,
                    SharedString::from(Os::CURRENT.reveal()),
                    reader_files::FileAction::Reveal,
                ),
                (
                    "file-copy",
                    IconName::Copy,
                    SharedString::from("Copy path"),
                    reader_files::FileAction::Absolute,
                ),
            ];
            #[cfg(not(target_os = "macos"))]
            let actions = vec![
                (
                    "file-open",
                    IconName::ExternalLink,
                    SharedString::from("Open with default app"),
                    reader_files::FileAction::Open,
                ),
                (
                    "file-reveal",
                    IconName::FolderOpen,
                    SharedString::from(Os::CURRENT.reveal()),
                    reader_files::FileAction::Reveal,
                ),
                (
                    "file-copy",
                    IconName::Copy,
                    SharedString::from("Copy path"),
                    reader_files::FileAction::Absolute,
                ),
            ];
            for (id, icon, tip, action) in actions {
                let root = root.clone();
                let rel = rel.clone();
                row = row.child(reader_icon_button(id, icon, tip, cx).on_click(
                    move |_, window, cx| reader_files::run(action, &root, &rel, window, cx),
                ));
            }
        }
        row.child(
            reader_icon_button("document-more", IconName::Ellipsis, "Document actions", cx)
                .debug_selector(|| "document-more".into())
                .dropdown_menu_with_anchor(Anchor::TopRight, move |mut menu, _, _| {
                    menu = menu
                        .menu_with_icon(
                            "Reveal in sidebar",
                            Icon::default().path(brand::READER_FOCUS_ICON),
                            Box::new(FocusCurrentFolder),
                        )
                        .separator();
                    if !is_file {
                        menu = menu.menu_with_icon(
                            "Find in note",
                            Icon::default().path("icons/text-search.svg"),
                            Box::new(FindInNote),
                        );
                        #[cfg(any(unix, windows))]
                        {
                            if editing {
                                if dirty {
                                    menu = menu.menu("Save", Box::new(SaveSource));
                                }
                                for (label, enabled) in [("Live Preview", true), ("Source", false)]
                                {
                                    let reader = reader.clone();
                                    menu = menu.item(PopupMenuItem::new(label).on_click(
                                        move |_, window, cx| {
                                            let _ = reader.update(cx, |this, cx| {
                                                this.set_live_preview(enabled, window, cx);
                                            });
                                        },
                                    ));
                                }
                            }
                            menu = menu
                                .menu(
                                    if editing { "Preview" } else { "Edit source" },
                                    Box::new(ToggleSource),
                                )
                                .item(
                                    PopupMenuItem::new("Rename")
                                        .action(Box::new(RenameNote))
                                        .on_click({
                                            let reader = reader.clone();
                                            move |_, window, cx| {
                                                let _ = reader.update(cx, |this, cx| {
                                                    this.rename_note_title(window, cx)
                                                });
                                            }
                                        }),
                                )
                                .menu("Move to…", Box::new(reader_move_picker::MoveToFolder))
                                .menu("Note history", Box::new(NoteSourceHistory))
                                .separator();
                        }
                        for (label, action) in [
                            (Os::CURRENT.reveal(), reader_files::FileAction::Reveal),
                            ("Copy path", reader_files::FileAction::Absolute),
                        ] {
                            let root = root.clone();
                            let rel = rel.clone();
                            menu = menu.item(PopupMenuItem::new(label).on_click(
                                move |_, window, cx| {
                                    reader_files::run(action, &root, &rel, window, cx)
                                },
                            ));
                        }
                    } else {
                        for (label, action) in [
                            ("Copy vault path", reader_files::FileAction::Relative),
                            ("Copy wikilink", reader_files::FileAction::Wiki),
                        ] {
                            let root = root.clone();
                            let rel = rel.clone();
                            menu = menu.item(PopupMenuItem::new(label).on_click(
                                move |_, window, cx| {
                                    reader_files::run(action, &root, &rel, window, cx)
                                },
                            ));
                        }
                    }
                    if !is_file {
                        menu = menu.menu("Open in new window", Box::new(reader_open::NewWindow));
                    }
                    #[cfg(unix)]
                    {
                        let reader = reader.clone();
                        let rel = rel.clone();
                        menu = menu
                            .separator()
                            .item(PopupMenuItem::new("Move to Trash").on_click(
                                move |_, window, cx| {
                                    let _ = reader.update(cx, |this, cx| {
                                        this.delete_path(rel.clone(), window, cx);
                                    });
                                },
                            ));
                    }
                    menu.separator().menu(
                        if is_file { "Close file" } else { "Close note" },
                        Box::new(CloseNote),
                    )
                }),
        )
    }

    pub(super) fn close_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_file().is_empty() {
            if self.save_source(cx) {
                window.remove_window();
            }
        } else {
            self.show_empty_vault(window, cx);
        }
    }

    pub(super) fn show_empty_vault(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.tree_preview.close();
        if !self.save_source(cx) {
            return;
        }
        if let Some(position) = self
            .navigation
            .history_positions
            .get_mut(self.navigation.history_ix)
        {
            *position = self.content.read(cx).list_state().logical_scroll_top();
        }
        if let Some(index) = self.navigation.history_nav {
            self.navigation.history_ix = index;
        } else if self
            .navigation
            .history
            .get(self.navigation.history_ix)
            .is_none_or(|entry| !entry.is_empty())
        {
            self.navigation
                .history
                .truncate(self.navigation.history_ix + 1);
            self.navigation
                .history_positions
                .truncate(self.navigation.history_ix + 1);
            self.navigation.history.push(String::new());
            self.navigation.history_positions.push(ListOffset {
                item_ix: 0,
                offset_in_item: px(0.),
            });
            self.navigation.history_ix = self.navigation.history.len() - 1;
        }
        self.close_quick_open(window, cx);
        self.clear_hover(cx);
        self.cancel_pending_landing();
        self.navigation.preparation_generation =
            self.navigation.preparation_generation.wrapping_add(1);
        self.navigation.generation = self.navigation.generation.wrapping_add(1);
        self.pending_open_document = None;
        self.timeline = None;
        self.current_rel.clear();
        self.current_title.clear();
        self.note_source.clear();
        self.note_canonical_source = None;
        self.file_preview = None;
        self.editing = None;
        self.table_overlay = None;
        self.find_open = false;
        self.usable_document = false;
        self.outline.clear();
        self.backlinks.clear();
        self.properties = Ok(Vec::new());
        self.link_notice = None;
        self.link_choices.clear();
        self.invalidate_links();
        self.link_presentations = Arc::default();
        self.link_original_source = None;
        self.link_identities.clear();
        self.last_recorded_document = None;
        if let Some(send) = &self.session_records {
            let _ = send.try_send(reader_loading::SessionRecord {
                root: self.vault_root.clone(),
                document: String::new(),
                single_file: self.single_file,
                cache: None,
                cache_lease: None,
                diagnostics: None,
            });
        }
        window.set_window_title("Tessera");
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    pub(super) fn render_empty_vault(&self, cx: &mut Context<Self>) -> AnyElement {
        let name = self
            .vault_root
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let mut recent = v_flex().gap_1();
        let recent_notes = self
            .sidebar
            .recent
            .iter()
            .filter(|(rel, _)| !self.tree.hidden_by_preference(rel));
        for (ix, item) in recent_notes.take(10).enumerate() {
            let rel = item.0.clone();
            let label = self.vault.note_title(&rel);
            recent = recent.child(
                Button::new(("empty-recent", ix))
                    .ghost()
                    .label(label)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_note(&rel, None, window, cx)
                    })),
            );
        }
        v_flex()
            .id("reader-empty-vault")
            .debug_selector(|| "reader-empty-vault".into())
            .size_full()
            .overflow_y_scroll()
            .p_8()
            .gap_4()
            .child(div().text_xl().child(name))
            .child(
                div()
                    .text_color(brand::palette(cx).text_muted)
                    .child(reader_shortcuts::hint("Search notes", &QuickOpen, cx)),
            )
            .when(cfg!(any(unix, windows)), |view| {
                view.child(
                    div()
                        .text_color(brand::palette(cx).text_muted)
                        .child(reader_shortcuts::hint("New note", &NewNote, cx)),
                )
            })
            .child(
                div()
                    .text_color(brand::palette(cx).text_muted)
                    .child(reader_shortcuts::hint(
                        "Keyboard shortcuts",
                        &ToggleShortcutSheet,
                        cx,
                    )),
            )
            .child(div().text_sm().child("Recent"))
            .child(recent)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[gpui::test]
    fn nested_note_title_keeps_space_before_optional_editor_controls(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        let relative = "Projects/Research/Archive/A reasonably long note title.md";
        std::fs::create_dir_all(root.join("Projects/Research/Archive")).unwrap();
        std::fs::write(
            root.join(relative),
            "# A reasonably long note title\n\nBody\n",
        )
        .unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
            reader_ui_state::install(&fixture.path().join("state"), cx);
            cx.set_global(reader_history::TestSessionDirectory(
                fixture.path().join("history"),
            ));
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let entity = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some(relative.into()),
                        index_dir: Some(fixture.path().join("index")),
                        panel_settings_override: Some(fixture.path().join("panels.json")),
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
        visual.simulate_resize(size(px(480.), px(900.)));
        visual.run_until_parked();
        let reader_title_width = visual
            .debug_bounds("reader-document-root")
            .unwrap()
            .size
            .width;
        reader.update_in(visual, |this, window, cx| {
            this.toggle_source(window, cx);
            this.set_live_preview(false, window, cx);
        });
        visual.run_until_parked();
        visual.simulate_input("Unsaved edits");
        for labels in [false, true] {
            visual.update(|_, cx| reader_ui_state::set_toolbar_labels(labels, cx));
            visual.run_until_parked();
            let title = visual.debug_bounds("reader-document-root").unwrap();
            let header = visual.debug_bounds("document-header").unwrap();
            assert!(
                title.size.width >= reader_title_width - px(40.),
                "dirty editor retains title space: {title:?}"
            );
            assert!(title.size.width > px(180.), "positive title-space control");
            for id in [
                "reader-history-back",
                "reader-history-forward",
                "reader-read",
                "reader-edit",
                "document-more",
            ] {
                let bounds = visual.debug_bounds(id).unwrap();
                assert!(
                    bounds.left() >= header.left() && bounds.right() <= header.right(),
                    "{id} clipped: {bounds:?}"
                );
                assert!(
                    bounds.right() <= title.left() || bounds.left() >= title.right(),
                    "{id} overlaps title"
                );
            }
            assert!(visual.debug_bounds("reader-live-preview").is_none());
            assert!(visual.debug_bounds("source-save").is_none());
            reader.read_with(visual, |this, cx| assert!(this.source_is_dirty(cx)));
        }
    }

    #[gpui::test]
    fn close_keeps_vault_back_restores_note_and_empty_selection_is_saved(cx: &mut TestAppContext) {
        let fixture = std::env::temp_dir().join(format!("tessera-close-{}", uuid::Uuid::new_v4()));
        let root = fixture.join("vault");
        let state = fixture.join("state");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("One.md"), "# One\n\nExact text\n").unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
            cx.set_global(reader_history::TestSessionDirectory(state.clone()));
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let entity = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("One.md".into()),
                        index_dir: Some(fixture.join("index")),
                        panel_settings_override: Some(fixture.join("panels.json")),
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
        reader.update_in(visual, |this, window, cx| {
            assert_eq!(this.current_rel, "One.md", "loaded-note positive control");
            this.focus_handle.focus(window, cx);
        });
        visual.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-w"
        } else {
            "ctrl-w"
        });
        reader.read_with(visual, |this, _| {
            assert!(this.current_rel.is_empty());
            assert_eq!(this.vault_root, root);
            assert_eq!(this.navigation.history, vec!["One.md", ""]);
        });
        visual.run_until_parked();
        assert!(visual.debug_bounds("reader-empty-vault").is_some());
        assert_eq!(
            reader_history::ReadingHistory::last_document(&state, &root)
                .unwrap()
                .as_deref(),
            Some("")
        );
        reader.update_in(visual, |this, window, cx| this.history_move(-1, window, cx));
        visual.run_until_parked();
        reader.update_in(visual, |this, _, _| assert_eq!(this.current_rel, "One.md"));
        assert!(visual.debug_bounds("reader-empty-vault").is_none());
        reader.update_in(visual, |this, window, cx| this.history_move(1, window, cx));
        visual.run_until_parked();
        assert!(visual.debug_bounds("reader-empty-vault").is_some());
        assert_eq!(
            std::fs::read_to_string(root.join("One.md")).unwrap(),
            "# One\n\nExact text\n"
        );
        assert!(
            visual.debug_bounds("reader-history-back").is_some(),
            "empty selection still offers document history navigation"
        );
        let before = visual.windows().len();
        visual.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-w"
        } else {
            "ctrl-w"
        });
        assert_eq!(
            visual.windows().len(),
            before - 1,
            "second Close closes the window"
        );
        let mut restored_reader = None;
        let (_, restored) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        index_dir: Some(fixture.join("index")),
                        panel_settings_override: Some(fixture.join("panels.json")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            restored_reader = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        restored.run_until_parked();
        assert!(
            restored.debug_bounds("reader-empty-vault").is_some(),
            "restart retains explicit empty selection despite notes in vault"
        );
        restored_reader.unwrap().read_with(restored, |reader, _| {
            assert!(
                reader.link_notice.is_none(),
                "empty selection is not a missing document"
            );
        });
        std::fs::remove_dir_all(fixture).unwrap();
    }

    #[gpui::test]
    fn document_header_stays_pinned_while_reader_and_source_scroll(cx: &mut TestAppContext) {
        let fixture = std::env::temp_dir().join(format!("tessera-header-{}", uuid::Uuid::new_v4()));
        let root = fixture.join("vault");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("Long.md"),
            format!("# Long\n\n{}", "Paragraph text.\n\n".repeat(150)),
        )
        .unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
            cx.set_global(reader_history::TestSessionDirectory(fixture.join("state")));
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let entity = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("Long.md".into()),
                        index_dir: Some(fixture.join("index")),
                        panel_settings_override: Some(fixture.join("panels.json")),
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
        visual.simulate_resize(size(px(1200.), px(860.)));
        visual.run_until_parked();
        let wheel = |visual: &mut VisualTestContext, delta| {
            visual.update(|window, cx| window.draw(cx).clear(cx));
            let body = visual.debug_bounds("reader-document").unwrap();
            visual.simulate_event(ScrollWheelEvent {
                position: point(body.center().x, body.top() + px(130.)),
                delta: ScrollDelta::Pixels(point(px(0.), px(delta))),
                ..Default::default()
            });
            visual.run_until_parked();
        };
        for source in [false, true]
            .into_iter()
            .filter(|source| !source || cfg!(any(unix, windows)))
        {
            reader.update_in(visual, |this, window, cx| {
                this.content
                    .read(cx)
                    .list_state()
                    .scroll_to(ListOffset::default());
                if source {
                    this.toggle_source(window, cx);
                }
                cx.notify();
            });
            visual.run_until_parked();
            let header = visual.debug_bounds("document-header-viewport").unwrap();
            assert_eq!(header.size.height, px(48.));
            let back = visual.debug_bounds("reader-history-back").unwrap();
            let forward = visual.debug_bounds("reader-history-forward").unwrap();
            let title = visual.debug_bounds("reader-document-root").unwrap();
            assert!(back.left() < forward.left() && forward.right() <= title.left());
            assert!(back.top() >= header.top() && back.bottom() <= header.bottom());
            for delta in [-24., -400.] {
                wheel(visual, delta);
                assert_eq!(
                    visual.debug_bounds("document-header-viewport").unwrap(),
                    header,
                    "header stays pinned in source={source}"
                );
                reader.read_with(visual, |this, cx| {
                    assert!(
                        !this.document_at_top(cx),
                        "positive control: first wheel scrolls content"
                    );
                });
            }
            if !source {
                reader.update_in(visual, |r, window, cx| {
                    r.content.read(cx).focus_handle().clone().focus(window, cx)
                });
                visual.run_until_parked();
                let before = reader.read_with(visual, |r, cx| {
                    r.content.read(cx).list_state().logical_scroll_top()
                });
                visual.simulate_keystrokes("pagedown");
                visual.run_until_parked();
                let after = reader.read_with(visual, |r, cx| {
                    r.content.read(cx).list_state().logical_scroll_top()
                });
                assert!(
                    after.item_ix > before.item_ix || after.offset_in_item > before.offset_in_item,
                    "positive control: Page Down moves the document"
                );
                assert_eq!(
                    visual.debug_bounds("document-header-viewport").unwrap(),
                    header
                );
            }
            wheel(visual, 10000.);
            wheel(visual, 100.);
            reader.read_with(visual, |r, cx| assert!(r.document_at_top(cx)));
            assert_eq!(
                visual.debug_bounds("document-header-viewport").unwrap(),
                header
            );
        }
        assert_eq!(
            std::fs::read_to_string(root.join("Long.md")).unwrap(),
            format!("# Long\n\n{}", "Paragraph text.\n\n".repeat(150))
        );
        std::fs::remove_dir_all(fixture).unwrap();
    }
}
