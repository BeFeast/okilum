use super::*;

pub(super) use okilum_shell_text::{reader_landing_state, ReaderLanding};

impl Reader {
    pub(super) fn open_note(
        &mut self,
        rel: &str,
        jump_term: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_note_at(rel, jump_term, None, window, cx);
    }

    /// `open_note`, scrolled to the heading `[[note#Heading]]` names (#49).
    /// A heading that is not in the note opens at the top, like Obsidian.
    pub(super) fn open_note_at(
        &mut self,
        rel: &str,
        jump_term: Option<&str>,
        heading: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_note_landing(rel, jump_term, heading, None, window, cx);
    }

    /// `open_note_at`, or at a file line from an external link (#1049).
    pub(super) fn open_note_landing(
        &mut self,
        rel: &str,
        jump_term: Option<&str>,
        heading: Option<&str>,
        landing: Option<prepared_links::Landing>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.ui_state.restoring_workspace {
            self.cancel_workspace_restore();
        }
        #[cfg(target_os = "macos")]
        let logical = reader_icloud::logical_path(Path::new(rel));
        #[cfg(target_os = "macos")]
        let rel = logical.to_str().unwrap_or(rel);
        if self.project_navigation.active.is_none() {
            self.close_project(window, cx);
        }
        self.project_navigation.section = reader_project_navigation::Section::Reader;
        self.tree_preview.close();
        if rel.is_empty() {
            self.show_empty_vault(window, cx);
            return;
        }
        if okilum_core::excalidraw::is_drawing(rel) || !rel.to_lowercase().ends_with(".md") {
            self.preview_file(rel, window, cx);
            return;
        }
        self.clear_hover(cx);
        self.pane.file_preview = None;
        if let Some(landing) = landing {
            self.prepare_link_document(rel, landing, heading, window, cx);
        } else {
            self.prepare_document(rel, jump_term, heading, window, cx);
        }
    }

    pub(super) fn accept_prepared_document(
        &mut self,
        request: prepared_links::DocumentRequest,
        document: anyhow::Result<prepared_links::PreparedDocument>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.accept_prepared_document_using(request, document, None, window, cx);
    }

    pub(super) fn accept_prepared_document_using(
        &mut self,
        request: prepared_links::DocumentRequest,
        document: anyhow::Result<prepared_links::PreparedDocument>,
        prepared_content: Option<Entity<TextViewState>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A note can disappear while the requested target is being prepared.
        // Root publications have already checked before changing vault_root.
        if prepared_content.is_none() {
            if self.leaving_deleted_note(Some(&request.rel), cx) {
                self.ask_leaving_deleted_note(
                    Box::new(move |this, window, cx| {
                        this.accept_prepared_document_using(
                            request,
                            document,
                            prepared_content,
                            window,
                            cx,
                        );
                    }),
                    window,
                    cx,
                );
                return;
            }
            if self.deleted_current().is_some() && request.rel == self.pane.current_rel {
                return;
            }
        }
        if self.pane.editing.is_some() {
            return;
        }
        self.clear_hover(cx);
        if request.rel != self.pane.current_rel {
            self.archive_lookup_notice = None;
            if self.single_file {
                self.refresh_quick_viewer_vault(cx);
            }
        }
        let prepared_links::PreparedDocument {
            canonical_source,
            encoding_damaged,
            built_in,
            source,
            original: original_body,
            identities,
            frontmatter,
            outline: prepared_outline,
        } = match document {
            Ok(document) => document,
            Err(error) => {
                diagnostic_error!("Document unavailable: {error:#}");
                #[cfg(target_os = "macos")]
                if let Some(error) = error.downcast_ref::<reader_icloud::DownloadError>() {
                    self.link_notice = Some(error.to_string().into());
                    cx.notify();
                    return;
                }
                self.link_notice = Some("This note couldn’t be opened. Check that it is available and you have permission to read it.".into());
                cx.notify();
                return;
            }
        };
        let rel = request.rel.as_str();
        let view = request.view.or_else(|| {
            request.history_index.and_then(|index| {
                let navigation = &self.pane.navigation;
                navigation
                    .history_views
                    .get(&index)
                    .filter(|_| navigation.history.get(index).map(String::as_str) == Some(rel))
                    .cloned()
            })
        });
        let jump_term = request.jump.as_deref();
        let heading = request.heading.as_deref();
        let heading_ix = if let Some(target) = heading {
            let result = if self.pane.use_html {
                Err("Heading navigation is unavailable in HTML mode.")
            } else {
                okilum_core::document_links::heading(&source, target).map(|h| h.block)
            };
            match result {
                Ok(ix) => Some(ix),
                Err(reason) => {
                    self.link_notice = Some(reason.into());
                    cx.notify();
                    return;
                }
            }
        } else {
            None
        };
        self.link_notice = None;
        self.pane.link_choices.clear();
        // A root publication clears history before replacing its old document;
        // do not attribute that old viewport to the newly published root.
        if self
            .pane
            .navigation
            .history
            .get(self.pane.navigation.history_ix)
            == Some(&self.pane.current_rel)
        {
            self.pane.navigation.places.insert(
                (self.vault_root.clone(), self.pane.current_rel.clone()),
                self.pane.navigation.pending_landing.unwrap_or_else(|| {
                    self.pane.content.read(cx).list_state().logical_scroll_top()
                }),
            );
        }
        self.cancel_pending_landing();
        self.pane.navigation.generation = self.pane.navigation.generation.wrapping_add(1);
        if request.history_index.is_none() {
            if let Some(position) = self
                .pane
                .navigation
                .history_positions
                .get_mut(self.pane.navigation.history_ix)
            {
                *position = self.pane.content.read(cx).list_state().logical_scroll_top();
            }
        }
        // Focus inside the table overlay returns to the document when a link
        // in the overlay navigates (#368).
        let content_had_focus = self
            .visible_markdown()
            .read(cx)
            .focus_handle()
            .is_focused(window)
            || self.pane.table_overlay.as_ref().is_some_and(|overlay| {
                overlay.read(cx).focus_handle().contains_focused(window, cx)
            });
        // The empty vault page draws no note, so `focus_document` gives the
        // Reader's own handle in its place; a note opened from there (a
        // compact navigator click, #677) takes that focus as it would from
        // the note it replaces (#2957). A root publication keeps the startup
        // focus on the Reader.
        let document_focus_held_by_reader = prepared_content.is_none()
            && self.empty_vault_shown()
            && self.focus_handle.is_focused(window);
        // A new state owns this document's parse/list. An old document's list
        // can never satisfy heading/Back readiness while replacement parses.
        self.pane.content = prepared_content.unwrap_or_else(|| {
            cx.new(|cx| {
                let state = if self.pane.use_html {
                    TextViewState::html("", cx)
                } else {
                    TextViewState::markdown("", cx)
                };
                state
                    .scrollable(true)
                    .selectable(true)
                    .selection_format(self.sel_format)
                    .retain_selection_on_layout(true)
            })
        });
        self.pane._content_sub = cx.observe(&self.pane.content, |this, _, cx| {
            this.record_usable_document(cx);
            this.content_changed(cx);
        });
        // Transfer focus only at replacement; asynchronous landing never steals it.
        if content_had_focus || document_focus_held_by_reader {
            window.focus(&self.pane.content.read(cx).focus_handle().clone(), cx);
        }
        self.invalidate_links();
        self.pane.link_presentations = Arc::default();
        self.pane.link_original_source = original_body.as_deref().map(Arc::from);
        self.pane.link_rendered_source = Some(Arc::from(source.as_str()));
        self.pane.link_identities = Arc::new(identities);
        let configured = reader_plugins(
            self.vault_root.clone(),
            TextView::new(&self.pane.content),
            cx.entity().downgrade(),
            self.sel_format,
            self.pane.link_presentations.clone(),
            &self.pane.link_identities,
            self.agent_document_enabled(cx),
        );
        {
            let _phase = reader_diagnostics::phase(cx, "note_open_set_text");
            self.pane.content.update(cx, |s, cx| {
                configured.prepare_state(s, cx);
                s.set_search_query("", cx);
                s.set_text_with_source(&source, original_body.map(SharedString::from), cx);
                if let Some(term) = jump_term {
                    s.set_search_query(term, cx);
                }
            });
        }
        self.trace_note_body(source.len(), window, cx);
        self.pane.outline = if self.pane.use_html {
            Vec::new()
        } else if let Some(outline) = prepared_outline {
            outline
        } else {
            let _phase = reader_diagnostics::phase(cx, "note_open_heading_inventory");
            note_outline(&source)
        };
        self.outline_list
            .reset_with_uniform_height(self.pane.outline.len(), px(OUTLINE_ROW_HEIGHT));
        self.pane.properties = frontmatter
            .as_deref()
            .map(okilum_core::properties::parse)
            .unwrap_or_else(|| Ok(Vec::new()));
        self.pane.typed_navigation = Default::default();
        self.pane.query_navigation = Default::default();
        self.pane.note_source = source;
        self.pane.note_canonical_source = canonical_source;
        self.pane.built_in_document = built_in;
        self.note_encoding_damaged = encoding_damaged;
        if !built_in && request.history_index.is_none() {
            self.quick_open.remember(rel);
        }
        if self.pane.current_rel != rel {
            self.pane.timeline = None;
        }
        // Every way of reaching another note (history, links, tree) ends up here:
        // the Tasks view of the reminders note ends when another note is shown.
        #[cfg(any(unix, windows))]
        if rel != self.reminder_prefs.note {
            self.reminders_view = false;
        }
        self.project_navigation.board_visible = false; // Accepted note; all existing save/navigation guards have passed.
        self.pane.current_rel = rel.to_string();
        self.pane.file_preview = if okilum_core::excalidraw::is_drawing(rel) {
            reader_files::FilePreview::load(&self.vault_root, rel)
                .ok()
                .map(|mut preview| {
                    let reader_edit = cx.entity().downgrade();
                    let reader_notify = reader_edit.clone();
                    preview.drawing = Some(cx.new(|_| {
                        reader_drawing::FileHandle::new(
                            Rc::new(move |path, window, cx| {
                                let _ = reader_edit
                                    .update(cx, |reader, cx| reader.edit_drawing(path, window, cx));
                            }),
                            Rc::new(move |cx| {
                                let _ = reader_notify.update(cx, |_, cx| cx.notify());
                            }),
                        )
                    }));
                    preview
                })
        } else {
            None
        };
        self.pane.table_overlay = None;
        self.discover_source_recovery(cx);
        if !built_in {
            self.note_opened(cx);
        }
        self.pane.usable_document = true;
        self.record_usable_document(cx);
        self.refresh_link_preparation(cx);
        self.pane.current_title = view
            .as_ref()
            .map_or_else(|| self.note_label(rel), |view| view.label.clone());
        self.set_backlinks(self.vault.backlinks(rel), true);
        self.show_window_title(Some(self.pane.current_title.clone()), window, cx);
        if request.history_index.is_none()
            && (heading.is_some()
                || self
                    .pane
                    .navigation
                    .history
                    .get(self.pane.navigation.history_ix)
                    .map(String::as_str)
                    != Some(rel)
                || self.pane.navigation.view_bindings() != view.as_ref())
        {
            // A new branch: forward entries are dropped, like a browser.
            self.pane.navigation.push_history(rel.to_owned(), view);
        }
        if self.pane.find_open {
            self.run_find(window, cx);
        }

        // Scroll to top now; if jumping, scroll again once the new text has
        // been parsed and the block list rebuilt (deferred, best-effort).
        self.pane
            .content
            .read(cx)
            .list_state()
            .scroll_to_reveal_item(0);
        // The reader's list has one item per top-level block, in source
        // order, so the heading's block index in the rendered source is its
        // item index. Plugins replace a block, never split or merge one.
        if let Some(ix) = heading_ix {
            self.scroll_to_block(ix, cx);
        }
        if let Some(index) = request.history_index {
            self.pane.navigation.history_ix = index;
        }
        if let Some(position) = request.restore_position {
            self.scroll_to_position(position, cx);
        }
        if self.shared_session.is_some() {
            self.restore_ui_state(window, cx);
        }
        cx.notify();
    }

    pub(super) fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pane.editing.is_none() || self.pane.file_preview.is_some() {
            if let Some(target) = self.viewer_find_target(cx) {
                target.focus(window, cx);
                return;
            }
        }
        if self.pane.file_preview.is_some() && self.pane.editing.is_none() {
            if let Some(code) = self
                .pane
                .file_preview
                .as_ref()
                .and_then(|p| p.code.as_ref())
            {
                code.update(cx, |code, cx| {
                    code.open_paged_find(window, cx);
                });
            }
            return;
        }
        self.quick_open.open = false;
        self.quick_open.invalidate();
        if self.pane.editing.is_some() {
            self.open_source_find(cx);
            return;
        }
        self.resizing_panel = None;
        // Docked panels stay while finding; a compact overlay would cover
        // the matches, so it closes.
        let available = f32::from(self.body_bounds.size.width);
        if reader_layout::overlay(available) {
            let active = self.panels.dismiss_target(available);
            self.panels.close(active);
        }
        self.pane.find_open = true;
        self.pane.find_input.update(cx, |s, cx| s.focus(window, cx));
        self.run_find(window, cx);
    }

    pub(super) fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pane.find_open = false;
        self.visible_markdown()
            .update(cx, |s, cx| s.set_search_query("", cx));
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    /// Recompute the matches for the find term and re-mark the note.
    pub(super) fn run_find(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.pane.typed_navigation.active.get() {
            self.pane
                .typed_navigation
                .scroll
                .set_offset(point(px(0.), px(0.)));
            cx.notify();
            return;
        }
        let term = self.pane.find_input.read(cx).value().trim().to_string();
        let sensitive = reader_ui_state::find_case_sensitive(cx);
        self.visible_markdown().update(cx, |s, cx| {
            s.set_search_case_sensitive(sensitive, cx);
            s.set_search_query(term, cx);
        });
        cx.notify();
    }

    pub(super) fn find_step(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.pane.typed_navigation.active.get() {
            cx.notify();
            return;
        }
        self.visible_markdown()
            .update(cx, |s, cx| s.step_search_match(delta, cx));
        cx.notify();
    }

    /// Scroll the reader so top-level block `ix` is at the top of the view,
    /// once the new text has been parsed (deferred, best-effort). Used for
    /// heading targets, where "at the top" is what a reader expects (#49).
    pub(super) fn scroll_to_block(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.scroll_to_position(
            ListOffset {
                item_ix: ix,
                offset_in_item: px(0.),
            },
            cx,
        );
    }

    pub(super) fn clear_landing_notice(&mut self) -> bool {
        if self.link_notice.as_deref().is_some_and(|notice| {
            notice.starts_with("The document did not finish rendering at the requested position.")
                || notice.starts_with("Could not render the requested position:")
        }) {
            self.link_notice = None;
            true
        } else {
            false
        }
    }

    pub(super) fn cancel_pending_landing(&mut self) {
        self.clear_landing_notice();
        self.pane.navigation.pending_landing = None;
        self.pane.navigation.landing_generation =
            self.pane.navigation.landing_generation.wrapping_add(1);
    }

    pub(super) fn scroll_to_position(&mut self, position: ListOffset, cx: &mut Context<Self>) {
        self.cancel_pending_landing();
        self.pane.navigation.pending_landing = Some(position);
        let landing = self.pane.navigation.landing_generation;
        let ticket = self.pane.navigation_ticket();
        let content = self.pane.content.entity_id();
        cx.spawn(async move |entity, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                let done = entity
                    .update(cx, |this, cx| {
                        // Started for another pane: leave this one's landing alone.
                        if !this.pane.owns(ticket) {
                            return true;
                        }
                        if landing != this.pane.navigation.landing_generation {
                            return true;
                        }
                        if !this.pane.holds_navigation(ticket)
                            || content != this.pane.content.entity_id()
                        {
                            this.pane.navigation.pending_landing = None;
                            return true;
                        }
                        if this.pane.typed_navigation.active.get() {
                            this.pane.navigation.pending_landing = None;
                            if let Err(reason) = reader_typed_view::land(this, position.item_ix) {
                                this.link_notice = Some(reason.into());
                            }
                            cx.notify();
                            return true;
                        }
                        if this.pane.query_navigation.pending.get() {
                            return false;
                        }
                        let query = this.pane.query_navigation.active.borrow().clone();
                        let (visible, position) = if let Some(query) = &query {
                            match query.landing(&this.pane.outline, position) {
                                Ok(position) => (query.text.clone(), position),
                                Err(reason) => {
                                    this.pane.navigation.pending_landing = None;
                                    this.link_notice = Some(reason.into());
                                    cx.notify();
                                    return true;
                                }
                            }
                        } else {
                            (this.pane.content.clone(), position)
                        };
                        // Parsing owns readiness. A hidden Reader (Source/Live
                        // session restoration) never renders its list, so waiting
                        // for item_count to grow can never finish. Publish the
                        // prepared list's position before its first layout instead.
                        match visible.read(cx).preparation_status() {
                            None => false,
                            Some(Err(reason)) => {
                                this.pane.navigation.pending_landing = None;
                                if this.pane.editing.is_none() {
                                    this.link_notice = Some(
                                        format!(
                                            "Could not render the requested position: {reason}"
                                        )
                                        .into(),
                                    );
                                }
                                cx.notify();
                                true
                            }
                            Some(Ok(())) => {
                                visible.update(cx, |s, cx| {
                                    s.scroll_to_prepared_position(position, cx);
                                });
                                this.pane.navigation.pending_landing = None;
                                let flash = this.pane.navigation.flash_pending.take();
                                if query.is_some() {
                                    cx.notify();
                                } else if flash == Some(position.item_ix) {
                                    this.flash_block(position.item_ix, cx);
                                }
                                true
                            }
                        }
                    })
                    .unwrap_or(true);
                if done {
                    break;
                }
            }
        })
        .detach();
    }

    pub(super) fn scroll_reader_key(
        &mut self,
        direction: f32,
        page: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pane.typed_navigation.active.get() && self.focus_handle.is_focused(window) {
            let distance = if page {
                self.pane.typed_navigation.scroll.bounds().size.height * 0.9
            } else {
                px(reader_ui_state::font_size(cx) * 2.)
            };
            let offset = self.pane.typed_navigation.scroll.offset();
            self.pane.typed_navigation.scroll.set_offset(point(
                offset.x,
                (offset.y - distance * direction).min(px(0.)),
            ));
            cx.notify();
            return;
        }
        if self.pane.editing.is_some()
            || self.pane.file_preview.is_some()
            || !self
                .visible_markdown()
                .read(cx)
                .focus_handle()
                .is_focused(window)
        {
            cx.propagate();
            return;
        }
        let visible = self.visible_markdown();
        let content = visible.read(cx);
        let distance = if page {
            (f32::from(content.list_state().viewport_bounds().size.height) * 0.9).max(40.)
        } else {
            reader_ui_state::font_size(cx) * 2.
        };
        content.list_state().scroll_by(px(direction * distance));
        cx.notify();
    }

    pub(super) fn history_move(
        &mut self,
        delta: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = self.pane.navigation.history_ix as isize + delta;
        if target < 0 || target as usize >= self.pane.navigation.history.len() {
            return;
        }
        self.pane.navigation.history_positions[self.pane.navigation.history_ix] =
            self.pane.content.read(cx).list_state().logical_scroll_top();
        let rel = self.pane.navigation.history[target as usize].clone();
        self.pane.navigation.history_nav = Some(target as usize);
        self.open_note(&rel, None, window, cx);
        self.pane.navigation.history_nav = None;
    }

    /// Move through the visible folder tree and open the note under the cursor.
    pub(super) fn list_move(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_tree();
        let tree = &self.tree;
        let rels: Vec<String> = tree
            .rows
            .iter()
            .filter(|row| row.kind == okilum_core::vault::EntryKind::Markdown)
            .map(|row| row.path.clone())
            .collect();
        if rels.is_empty() {
            return;
        }
        let cur = rels.iter().position(|r| *r == self.pane.current_rel);
        let next = match cur {
            Some(ix) => (ix as isize + delta).clamp(0, rels.len() as isize - 1) as usize,
            None if delta < 0 => rels.len() - 1,
            None => 0,
        };
        if Some(next) != cur {
            if self
                .panels
                .dismiss_target(f32::from(self.body_bounds.size.width))
                == reader_layout::Panel::Closed
            {
                self.open_note(&rels[next], None, window, cx);
            } else {
                self.select_panel_note(reader_layout::Panel::Notes, &rels[next], None, window, cx);
            }
        }
    }

    pub(super) fn select_panel_note(
        &mut self,
        panel: reader_layout::Panel,
        rel: &str,
        jump: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_note(rel, jump, window, cx);
        if panel == reader_layout::Panel::Notes {
            self.dismiss_sidebar_after_selection(window, cx);
        }
        self.focus_document(window, cx);
        cx.notify();
    }

    /// Navigation dismisses a compact overlay without changing the wide layout.
    pub(super) fn dismiss_sidebar_after_selection(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let width = f32::from(self.body_bounds.size.width);
        if reader_layout::overlay(width) && self.panels.visible(reader_layout::Panel::Notes, width)
        {
            self.panels.active = reader_layout::Panel::Closed;
            self.resizing_panel = None;
            self.focus_document(window, cx);
            cx.notify();
        }
    }

    pub(super) fn render_find_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let native = self.pane.typed_navigation.active.get();
        let (current, total) = self.visible_markdown().read(cx).search_status();
        let count = if native {
            "Filter tasks".to_owned()
        } else if total == 0 {
            if self.pane.find_input.read(cx).value().trim().is_empty() {
                String::new()
            } else {
                "No matches".to_string()
            }
        } else {
            format!("{} of {}", current + 1, total)
        };
        let weak = cx.entity().downgrade();
        let case = weak.clone();
        let step = weak.clone();
        let target = reader_find_field::Target {
            input: self.pane.find_input.clone(),
            focus_override: None,
            scope: "Find in note…".into(),
            count: Some(count.into()),
            match_case: Some((
                reader_ui_state::find_case_sensitive(cx),
                std::rc::Rc::new(move |window, cx| {
                    let _ = case.update(cx, |this, cx| {
                        let sensitive = !reader_ui_state::find_case_sensitive(cx);
                        reader_ui_state::set_find_case_sensitive(sensitive, cx);
                        this.run_find(window, cx);
                    });
                }),
            )),
            step: Some(std::rc::Rc::new(move |direction, _, cx| {
                let _ = step.update(cx, |this, cx| this.find_step(direction as isize, cx));
            })),
            steps_visible: !native,
            close: std::rc::Rc::new(move |window, cx| {
                let _ = weak.update(cx, |this, cx| this.close_find(window, cx));
            }),
            large_file: false,
        };
        reader_find_field::render(&target, reader_find_field::Presentation::Floating, cx)
    }
}
