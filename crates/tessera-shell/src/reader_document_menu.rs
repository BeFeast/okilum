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
            || self.selected_file().is_empty()
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
                    .child(self.render_document_header(cx)),
            )
            .child(div().flex_1().min_h_0().child(self.render_main(window, cx)))
            .into_any_element()
    }

    pub(super) fn render_document_header(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
        let is_file = self.file_preview.is_some();
        #[cfg(any(unix, windows))]
        let editing = self.editing.is_some();
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
            .child(self.render_breadcrumbs(cx));
        #[cfg(any(unix, windows))]
        if !is_file {
            use gpui_component::button::ButtonGroup;
            let labels = reader_ui_state::toolbar_labels(cx);
            row = row.child(
                ButtonGroup::new("note-mode").children([
                    Button::new("reader-read")
                        .ghost()
                        .small()
                        .icon(IconName::BookOpen)
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
                        .when(labels, |button| button.label("Edit"))
                        .selected(editing)
                        .accessibility_label("Edit")
                        .tooltip(reader_shortcuts::hint("Edit", &ToggleSource, cx))
                        .debug_selector(|| "reader-edit".into())
                        .on_click(cx.listener(|this, _, window, cx| {
                            if this.editing.is_none() {
                                this.toggle_source(window, cx);
                            }
                        })),
                ]),
            );
            if editing {
                row = row.child(self.render_live_preview_control(cx));
                if self.source_is_dirty(cx) {
                    row = row.child(self.render_save_status(cx)).child(
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
        if !is_file {
            row = row
                .child(
                    reader_icon_button(
                        "note-reveal",
                        Icon::default().path(brand::READER_FOCUS_ICON),
                        "Reveal in sidebar",
                        cx,
                    )
                    .on_click(
                        cx.listener(|this, _, window, cx| this.focus_current_folder(window, cx)),
                    ),
                )
                .child(
                    reader_icon_button(
                        "note-find",
                        Icon::default().path("icons/text-search.svg"),
                        with_shortcut("Find in note", "secondary-f"),
                        cx,
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.open_find(window, cx))),
                );
            #[cfg(any(unix, windows))]
            {
                row = row.child(
                    reader_icon_button(
                        "note-rename",
                        Icon::default().path("icons/pencil.svg"),
                        "Rename",
                        cx,
                    )
                    .on_click(
                        cx.listener(|this, _, window, cx| this.rename_note_title(window, cx)),
                    ),
                );
            }
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
                            menu = menu
                                .menu(
                                    if editing { "Preview" } else { "Edit source" },
                                    Box::new(ToggleSource),
                                )
                                .menu("Rename…", Box::new(RenameNote))
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
                    // #466's Delete action joins this document-scoped menu once available.
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
        if !self.save_source(cx) {
            return;
        }
        if let Some(position) = self.history_positions.get_mut(self.history_ix) {
            *position = self.content.read(cx).list_state().logical_scroll_top();
        }
        if let Some(index) = self.history_nav {
            self.history_ix = index;
        } else if self
            .history
            .get(self.history_ix)
            .is_none_or(|entry| !entry.is_empty())
        {
            self.history.truncate(self.history_ix + 1);
            self.history_positions.truncate(self.history_ix + 1);
            self.history.push(String::new());
            self.history_positions.push(ListOffset {
                item_ix: 0,
                offset_in_item: px(0.),
            });
            self.history_ix = self.history.len() - 1;
        }
        self.close_quick_open(window, cx);
        self.clear_hover(cx);
        self.cancel_pending_landing();
        self.document_preparation_generation = self.document_preparation_generation.wrapping_add(1);
        self.navigation_generation = self.navigation_generation.wrapping_add(1);
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
            assert_eq!(this.history, vec!["One.md", ""]);
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
