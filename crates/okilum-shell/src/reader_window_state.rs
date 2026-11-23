use super::*;

impl Reader {
    /// Share the enclosing Workspace chrome without changing document state.
    #[cfg(all(unix, feature = "brain"))]
    pub(crate) fn embedded_in_workspace(mut self) -> Self {
        self.embedded_in_workspace = true;
        self
    }

    pub(super) fn new(mut opts: Opts, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let _constructor_phase = reader_diagnostics::phase(cx, "reader_constructor");
        if opts.diagnostics.is_none() {
            opts.diagnostics = reader_diagnostics::trace(cx);
        }
        if let (Some(trace), Some(root)) = (&opts.diagnostics, &opts.vault) {
            opts.diagnostics = Some(trace.for_root(root.clone()));
        }
        if opts.session_directory.is_none() {
            #[cfg(not(test))]
            {
                opts.session_directory = reader_history::state_directory().ok();
            }
            #[cfg(test)]
            {
                opts.session_directory = cx
                    .try_global::<reader_history::TestSessionDirectory>()
                    .map(|d| d.0.clone());
            }
        }
        #[cfg(windows)]
        if let (Some(state), Some(root)) = (&opts.session_directory, &opts.vault) {
            let drafts = state.join("editor-drafts");
            let root = root.clone();
            cx.background_executor()
                .spawn(async move {
                    match okilum_core::source_history::cleanup_windows(&drafts, &root) {
                        Ok(warnings) => {
                            for warning in warnings {
                                diagnostic_error!("{warning}");
                            }
                        }
                        Err(error) => {
                            diagnostic_error!("Save recovery cleanup retained files: {error:#}")
                        }
                    }
                })
                .detach();
        }
        let use_html = opts.use_html;
        let vault_root = opts.vault.clone().unwrap_or_default();
        let index_dir = opts.index_dir.clone();
        let mut placeholder = Vault::from_note_paths([]);
        placeholder.root = vault_root.clone();
        let vault = Arc::new(placeholder);
        let searcher = None;
        let quick_open = quick_open::Palette::new(window, cx);
        let shortcut_sheet = reader_shortcuts::Sheet::new(window, cx);
        let find_input = cx.new(|cx| InputState::new(window, cx).placeholder("Find in note…"));
        let sel_format = if opts.copy_source {
            SelectionFormat::Source
        } else {
            SelectionFormat::Plain
        };
        let content = cx.new(|cx| {
            if use_html {
                TextViewState::html("", cx)
                    .retain_selection_on_layout(true)
                    .scrollable(true)
                    .selectable(true)
            } else {
                TextViewState::markdown("", cx)
                    .retain_selection_on_layout(true)
                    .scrollable(true)
                    .selectable(true)
                    .selection_format(sel_format)
            }
        });
        // Enter on a single-line input propagates the key and emits
        // PressEnter, so cycling the matches hangs off the event rather than
        // off a binding that would have to outrank the input's own.
        let find_sub = cx.subscribe_in(
            &find_input,
            window,
            |this: &mut Reader, _, e: &InputEvent, window, cx| match e {
                InputEvent::Change => this.run_find(window, cx),
                InputEvent::PressEnter { shift: true, .. } => this.find_step(-1, cx),
                InputEvent::PressEnter { .. } => this.find_step(1, cx),
                _ => {}
            },
        );

        let content_sub = cx.observe(&content, |this, _, cx| this.content_changed(cx));

        #[cfg(target_os = "linux")]
        let header_cache = {
            let parent = cx.weak_entity();
            cx.new(|cx| reader_header_cache::Header::new(parent, cx))
        };
        #[cfg(target_os = "linux")]
        let layout_signal = cx.new(|_| ());
        let mut this = Self {
            #[cfg(target_os = "linux")]
            header_cache,
            #[cfg(target_os = "linux")]
            layout_signal,
            #[cfg(target_os = "linux")]
            panel_capture: Default::default(),
            ui_state: Default::default(),
            loading: None,
            queued_edit_note: None,
            pending_link_edit: None,
            side_panels: [None, None],
            outline_section: None,
            content_status: None,
            shared_session: None,
            shared_version: 0,
            shared_service_version: 0,
            vault_publication_revision: (0, 0),
            source_publication_epoch: 0,
            open_started: None,
            note_body_trace: None,
            session_directory: opts.session_directory.clone(),
            session_records: None,
            title_note: None,
            index_dir,
            cache_lease: None,
            vault,
            searcher,
            vault_root: vault_root.clone(),
            watcher: None,
            quick_folders: Default::default(),
            quick_folder_refreshes: Default::default(),
            watcher_generation: 0,
            open_note_watch: None,
            watcher_poll_active: false,
            #[cfg(test)]
            watcher_poll_hold: None,
            deferred_vault_changes: Default::default(),
            incremental_state: None,
            tasks_index: None,
            incremental_initializing: false,
            incremental_active: false,
            incremental_epoch: 0,
            incremental_cancel: None,
            #[cfg(test)]
            incremental_hold: None,
            single_file: false,
            reader_window: window.window_handle(),
            sidebar_search_focus: cx.focus_handle(),
            recent_switcher: Default::default(),
            quick_open,
            shortcut_sheet,
            zoom_hud: Default::default(),
            viewer_save_controls: Default::default(),
            viewer_header: Default::default(),
            viewer_save_bounds: Default::default(),
            viewer_find_controls: Default::default(),
            viewer_find_bounds: Default::default(),
            viewer_find_min_x: Default::default(),
            #[cfg(target_os = "linux")]
            sync_note_conflicts: None,
            retained_edit: None,
            #[cfg(any(unix, windows))]
            creation: None,
            #[cfg(any(unix, windows))]
            creation_undo: None,
            #[cfg(any(unix, windows))]
            reminder_undo: None,
            #[cfg(any(unix, windows))]
            reminder_notifier: Default::default(),
            #[cfg(any(unix, windows))]
            reminder_prefs: Default::default(),
            #[cfg(any(unix, windows))]
            reminder_prefs_error: None,
            #[cfg(any(unix, windows))]
            daily_notes: None,
            #[cfg(any(unix, windows))]
            daily_notes_error: None,
            #[cfg(any(unix, windows))]
            daily_templates: Default::default(),
            #[cfg(any(unix, windows))]
            daily_setup: None,
            #[cfg(any(unix, windows))]
            reminders_view: false,
            #[cfg(any(unix, windows))]
            renaming: None,
            #[cfg(any(unix, windows))]
            move_picker: Default::default(),
            batch: Default::default(),
            #[cfg(any(unix, windows))]
            note_move_pending: false,
            #[cfg(any(unix, windows))]
            move_notice_generation: 0,
            #[cfg(any(unix, windows))]
            move_applying: false,
            #[cfg(any(unix, windows))]
            trash_pending: false,
            #[cfg(any(unix, windows))]
            trash_undo: reader_trash::UndoHistory::default(),
            #[cfg(any(unix, windows))]
            move_index: None,
            link_notice: None,
            displayed_deleted_notice: None,
            deleted_notice_generation: 0,
            deleted_note_asking: false,
            displayed_notice: None,
            displayed_choices: Vec::new(),
            displayed_recovery: None,
            recovery_toast: None,
            recovery_toast_generation: 0,
            pointer_link: None,
            read_press: None,
            displayed_history_notice: None,
            history_notice_generation: 0,
            notice_generation: 0,
            toast_subscription: None,
            pane: reader_pane::Surface::new(
                content,
                content_sub,
                find_input,
                use_html,
                cx.try_global::<reader_recovery::RecoveryStartup>()
                    .is_some_and(|state| state.0),
            ),
            backlinks_list: ListState::new(0, ListAlignment::Top, px(200.)),
            stignore: None,
            stignore_offered: Default::default(),
            stignore_busy: false,
            outline_list: ListState::new(0, ListAlignment::Top, px(200.)),
            perf_probe: reader_perf_probe::Probe::from_env(),
            sel_format,
            focus_handle: cx.focus_handle(),
            note_encoding_damaged: false,
            encoding_repair_pending: None,
            draft_outline: Default::default(),
            tree: reader_tree::Tree::default(),
            tree_source: None,
            tree_rules_source: None,
            tree_rules_running: false,
            tree_revealed: String::new(),
            tree_auto_reveal: false,
            tree_wheel_at: None,
            tree_reveal_waiting: false,
            tree_reveal_retry: None,
            tree_index_entry: None,
            tree_preview: Default::default(),
            tree_sticky: Vec::new(),
            tree_earlier_roots: Vec::new(),
            project: None,
            project_open: false,
            project_navigation: Default::default(),
            title_nav: Default::default(),
            project_request: 0,
            project_views: Default::default(),
            file_menu: None,
            tree_focus: cx.focus_handle(),
            tree_scroll: UniformListScrollHandle::new(),
            sidebar: reader_sidebar::State::default(),
            sidebar_path: None,
            agents_offer: reader_agents_offer::Offer::default(),
            agent_document: None,
            agent_language: None,
            _agent_language_events: None,
            agent_language_marks: None,
            hover_revision: None,
            inbox: Vec::new(),
            projects: Arc::default(),
            workbench: Arc::default(),
            workbench_ready: None,
            workbench_pending: 0,
            workbench_clock_minute: 0,
            workbench_times: Default::default(),
            workbench_scroll: ScrollHandle::new(),
            waiting: Default::default(),
            inbox_source: None,
            recent_expanded: false,
            sidebar_save_sequence: Arc::default(),
            inbox_ready_root: None,
            backlink_titles: Arc::default(),
            // docs/design/reader.md: from R2 the sidebar starts open where it
            // docks; compact windows still start with every panel closed.
            // Tests keep the accepted closed default (#321).
            panels: reader_layout::Panels {
                notes: cfg!(not(test)) && !reader_recovery::is_recovering(cx),
                backlinks: cfg!(not(test)) && !reader_recovery::is_recovering(cx),
                ..Default::default()
            },
            embedded_in_workspace: false,
            panel_widths: reader_layout::Widths::default(),
            panel_settings: None,
            panel_preferences_ready: false,
            panel_widths_revision: 0,
            body_bounds: Bounds::default(),
            body_viewport_width: window.viewport_size().width,
            resizing_panel: None,
            refocus_if_lost: false,
            split: None,
            _subs: vec![find_sub],
        };
        let weak = cx.weak_entity();
        this._subs
            .push(
                window.observe_global::<reader_features::Features>(cx, move |window, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.features_changed(window, cx);
                        this.sync_agent_document(cx);
                    });
                }),
            );
        this._subs
            .push(cx.on_focus_lost(window, |this, window, cx| {
                this.refocus_lost_field(window, cx)
            }));
        this.side_panels = reader_side_panel::SidePanel::pair(cx);
        // Nothing has focus on a fresh window, and gpui then dispatches keys
        // to the root node only, where the reader's context is not on the
        // stack. Take focus so the bindings are live from the first keystroke.
        window.focus(&this.focus_handle, cx);
        // Poll the watcher on a timer. notify delivers on its own thread; the
        // UI must not be touched from there, so events are drained here on the
        // main thread instead. 250 ms is below the watcher's own quiet window,
        // so it never adds latency of its own.
        cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(250))
                .await;
            let alive = this
                .update_in_own_window(cx, |this, window, cx| this.poll_vault(window, cx))
                .is_ok();
            if !alive {
                break;
            }
        })
        .detach();

        if let Some(query) = &opts.query {
            this.open_quick_open(true, window, cx);
            this.quick_open
                .input
                .update(cx, |input, cx| input.set_value(query.clone(), window, cx));
            this.refresh_quick_open(cx);
        }
        cx.observe_self(|this, cx| this.record_ui_state(this.ui_state.was_active(), cx))
            .detach();
        cx.observe_window_activation(window, |this, window, cx| {
            // Re-entry can precede a compositor's delayed modifier release.
            // Never carry an unfinished gesture across activation boundaries.
            this.recent_switcher.cancel();
            #[cfg(target_os = "linux")]
            if !window.is_window_active() && this.resizing_panel.take().is_some() {
                this.release_panel_pointer(window);
            }
            cx.notify();
            this.record_ui_state(window.is_window_active(), cx);
            if window.is_window_active() {
                reader_open::focused(cx.entity().downgrade(), cx);
                reader_open_windows::focused(window.window_handle(), cx);
            }
        })
        .detach();
        this.install_source_lifecycle(window, cx);
        this.watch_open_note(cx);
        this.start_session_records(cx);
        #[cfg(any(unix, windows))]
        if let Some(state) = &this.session_directory {
            this.reminder_prefs = reader_reminder_settings::load(state, &this.vault_root);
        }
        if !opts.defer_loading {
            this.start_loading(opts, window, cx);
        }
        this
    }

    /// Drain/coalesce notifications off the UI thread. Known note batches use
    /// the reconciled baseline, including directory hints. Overflow needs reconcile.
    pub(super) fn poll_vault(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_workbench_clock(cx);
        if self.shared_session.is_some() {
            if !self.loading.as_ref().is_some_and(|load| load.active) {
                self.poll_shared_session(window, cx);
            }
            return;
        }
        if self.loading.as_ref().is_some_and(|l| l.active)
            || self.incremental_active
            || self.incremental_initializing
        {
            return;
        }
        if !self.deferred_vault_changes.is_empty() {
            let changes = std::mem::take(&mut self.deferred_vault_changes);
            self.apply_vault_changes(changes, window, cx);
            return;
        }
        let Some(mut watcher) = self.watcher.take() else {
            return;
        };
        #[cfg(test)]
        let hold = self.watcher_poll_hold.take();
        self.watcher_poll_active = true;
        let generation = self.watcher_generation;
        let root = self.vault_root.clone();
        let quick_folders = self.single_file.then(|| self.quick_folders.clone());
        cx.spawn_in(window, async move |this, cx| {
            let (watcher, changes, error) = cx
                .background_executor()
                .spawn(async move {
                    #[cfg(test)]
                    if let Some(hold) = hold {
                        let _ = hold.recv().await;
                    }
                    let error = quick_folders
                        .as_ref()
                        .and_then(|folders| watcher.watch_folders(folders).err());
                    let changes = watcher.poll();
                    (watcher, changes, error)
                })
                .await;
            let _ = this.update_in_own_window(cx, |this, window, cx| {
                if this.vault_root != root
                    || this.watcher_generation != generation
                    || this.watcher.is_some()
                {
                    return;
                }
                this.watcher_poll_active = false;
                this.watcher = Some(watcher);
                if let Some(error) = error {
                    let message = format!("Automatic folder updates unavailable: {error}");
                    if this.link_notice.as_deref() != Some(message.as_str()) {
                        this.link_notice = Some(message.into());
                        cx.notify();
                    }
                }
                if let Some(mut changes) = changes {
                    // The policy is no note: refresh the open agent file's
                    // language check now and keep it out of index updates.
                    if std::mem::take(&mut changes.language_policy) {
                        this.agent_language_policy_changed(cx);
                        if changes.is_empty() {
                            return;
                        }
                    }
                    if this.loading.as_ref().is_some_and(|load| load.active) {
                        this.deferred_vault_changes
                            .metadata
                            .extend(changes.metadata);
                        this.deferred_vault_changes.rescan |= changes.rescan;
                        this.deferred_vault_changes
                            .directories
                            .extend(changes.directories);
                        this.deferred_vault_changes.changed.extend(changes.changed);
                        this.deferred_vault_changes.removed.extend(changes.removed);
                    } else {
                        this.apply_vault_changes(changes, window, cx);
                    }
                }
            });
        })
        .detach();
    }

    pub(super) fn apply_vault_changes(
        &mut self,
        mut changes: okilum_core::Changes,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !changes.metadata.is_empty() && self.shared_session.is_none() {
            self.refresh_service_metadata(window, cx);
            changes.metadata.clear();
            if changes.is_empty() {
                return;
            }
        }
        if !changes.is_empty() {
            self.invalidate_links();
            // A newer source hint revokes an older document reconciliation,
            // including its late attempt to prepare actionable link evidence.
            self.pane.navigation.reconciliation_generation = self
                .pane
                .navigation
                .reconciliation_generation
                .wrapping_add(1);
        }
        if self.shared_session.is_some() {
            self.deferred_vault_changes
                .metadata
                .extend(changes.metadata);
            self.deferred_vault_changes.rescan |= changes.rescan;
            self.deferred_vault_changes.changed.extend(changes.changed);
            self.deferred_vault_changes.removed.extend(changes.removed);
            self.deferred_vault_changes
                .directories
                .extend(changes.directories);
            self.poll_vault(window, cx);
            return;
        }
        self.refresh_project_resources(&changes, cx);
        reader_drawing::invalidate(&self.vault_root, cx);
        if let Some(viewer) = self.pdf_viewer().cloned() {
            viewer.update(cx, |viewer, cx| viewer.check_revision(cx));
        }
        if self.incremental_active || self.incremental_initializing {
            self.deferred_vault_changes
                .metadata
                .extend(changes.metadata);
            self.deferred_vault_changes.rescan |= changes.rescan;
            self.deferred_vault_changes
                .directories
                .extend(changes.directories);
            self.deferred_vault_changes.changed.extend(changes.changed);
            self.deferred_vault_changes.removed.extend(changes.removed);
            return;
        }
        if self.single_file {
            self.refresh_quick_folders(changes, window, cx);
            return;
        }
        if !changes.is_empty()
            && !changes.rescan
            && self.incremental_state.is_some()
            && self.searcher.is_some()
        {
            self.start_incremental(changes, window, cx);
        } else {
            self.refresh_inventory(changes, window, cx);
        }
    }

    /// Escape clears or dismisses local transient UI; it never toggles panels (#483).
    /// A non-empty focused search field is cleared and keeps focus; an empty one closes.
    pub(super) fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.title_nav.menu.is_some() {
            self.close_nav_menu(window, cx);
            return;
        }
        if self.recent_switcher.open() {
            self.recent_switcher.cancel();
            cx.notify();
            return;
        }
        if self.tree_preview.is_open() {
            self.tree_preview.close();
            self.tree_focus.focus(window, cx);
            cx.notify();
            return;
        }
        #[cfg(any(unix, windows))]
        if self.renaming.is_some() {
            self.cancel_rename(window, cx);
            return;
        }
        #[cfg(any(unix, windows))]
        if self.creation.is_some() {
            self.cancel_creation(window, cx);
            return;
        }
        let log = self
            .pane
            .file_preview
            .as_ref()
            .and_then(|preview| preview.log.clone());
        if log.is_some_and(|log| log.update(cx, |log, cx| log.dismiss_saving(window, cx))) {
            return;
        }
        if self.pane.hover_preview.is_active() {
            self.clear_hover(cx);
            return;
        }
        if self.shortcut_sheet.open {
            self.dismiss_shortcut_sheet(window, cx);
            return;
        }
        if self.quick_open.open {
            if self.quick_open.input.read(cx).value().is_empty() {
                self.close_quick_open(window, cx);
            } else {
                self.quick_open.input.update(cx, |input, cx| {
                    input.set_value("", window, cx);
                    input.focus(window, cx);
                });
                self.refresh_quick_open(cx);
            }
            return;
        }
        if self.pane.table_overlay.is_some() {
            self.close_table_overlay(window, cx);
            return;
        }
        if self.active_timeline().is_some_and(|t| t.selected.is_some()) {
            self.back_from_timeline(window, cx);
            return;
        }
        if self.loading.as_ref().is_some_and(|l| l.active) {
            self.cancel_loading(cx);
            return;
        }
        let find_focused = self
            .pane
            .find_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        if self.pane.find_open && find_focused {
            if self.pane.find_input.read(cx).value().is_empty() {
                self.close_find(window, cx);
            } else {
                self.pane
                    .find_input
                    .update(cx, |input, cx| input.set_value("", window, cx));
                self.run_find(window, cx);
            }
            return;
        }
        #[cfg(unix)]
        if self.dismiss_trash_toast(window, cx) {
            return;
        }
        if reader_toast::dismiss(window, cx) {
            return;
        }
        if self.pane.find_open {
            self.close_find(window, cx);
            return;
        }
        // Esc in the tree with nothing else to close deselects, so the next
        // New note / New folder goes to the root (#1512).
        if self.tree_focus.is_focused(window) && self.tree.cursor.is_some() {
            self.clear_tree_selection(cx);
            return;
        }
        // #1642: an open Live Preview block is drawn rendered again first.
        if self.close_rendered_block(cx) {
            return;
        }
        #[cfg(any(unix, windows))]
        if self.pane.editing.is_some() {
            self.toggle_source(window, cx);
        }
    }

    pub(super) fn set_panel_width(&mut self, panel: reader_layout::Panel, width: f32) {
        self.ui_state.interacted = true;
        self.panel_widths_revision = self.panel_widths_revision.wrapping_add(1);
        self.panel_widths.set(panel, width);
    }

    pub(super) fn persist_panel_width(&mut self, panel: reader_layout::Panel) {
        if self.ui_state.ready {
            return;
        }
        if let Some(path) = &self.panel_settings {
            if let Err(error) = self.panel_widths.save_panel(panel, path) {
                self.link_notice = Some(format!("Could not save panel width: {error}").into());
            }
        }
    }

    pub(super) fn close_panel(
        &mut self,
        panel: reader_layout::Panel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.resizing_panel = None;
        self.ui_state.interacted = true;
        #[cfg(native_manager)]
        if panel == reader_layout::Panel::Backlinks {
            if let Some(project) = &self.project {
                project.update(cx, |project, cx| project.manager_surface_visible(false, cx));
            }
        }
        self.panels.close(panel);
        self.focus_after_layout_change(window, cx);
        cx.notify();
    }

    pub(super) fn toggle_panel(
        &mut self,
        panel: reader_layout::Panel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if panel == reader_layout::Panel::Notes && self.project_navigation.active.is_some() {
            self.show_vault_files(window, cx);
            return;
        }
        self.ui_state.interacted = true;
        self.resizing_panel = None;
        if self
            .panels
            .visible(panel, f32::from(self.body_bounds.size.width))
        {
            self.close_panel(panel, window, cx);
        } else {
            self.panels.open(panel);
            if panel == reader_layout::Panel::Notes {
                self.focus_sidebar_search(window, cx);
            } else {
                self.focus_handle.focus(window, cx);
                cx.notify();
            }
        }
    }

    pub(super) fn maximize_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.resizing_panel = None;
        self.ui_state.interacted = true;
        self.panels.toggle_maximized();
        // Presentation only: keep the current editor, selection, draft and history.
        self.focus_after_layout_change(window, cx);
        cx.notify();
    }

    /// Maximize note and a panel hide (chord, toolbar, overlay backdrop or
    /// menu) only change the layout, so they leave focus wherever it is drawn:
    /// in the document or viewer, Find, a rename, Quick Open. The
    /// document takes focus only when nothing drawn holds it: no focus, or a
    /// handle the last frame did not draw (#2957), now; or, after the next
    /// frame, an element this change stopped drawing (a rename when Maximize
    /// hides the Notes panel), through `refocus_lost_field`. These actions
    /// reach the Reader along the focused element's dispatch path or from a
    /// click inside it, so drawn focus outside the Reader does not occur here.
    ///
    /// Two drawn holders are not a place the user is working in, and hand
    /// focus to the document as before #2957:
    /// - the Reader's own handle, which only stands in for the document
    ///   (opening a right panel or closing Find puts focus there);
    ///   `focus_document` keeps it where the Reader handle is the document's
    ///   focus (a file viewer, the empty vault page);
    /// - the vault search entry once Notes is hidden: the header always draws
    ///   it, but it is the Notes panel's entry point (opening Notes focuses
    ///   it), as a resize that hides Notes already treats it.
    fn focus_after_layout_change(&mut self, window: &mut Window, cx: &mut App) {
        let reader_stand_in = self.focus_handle.is_focused(window);
        let search_without_notes = self.sidebar_search_focus.is_focused(window)
            && !self.panels.visible(
                reader_layout::Panel::Notes,
                f32::from(self.body_bounds.size.width),
            );
        if focus_drawn_in_reader(window) && !reader_stand_in && !search_without_notes {
            self.refocus_if_lost = true;
        } else {
            self.refocus_if_lost = false;
            self.focus_document(window, cx);
        }
    }

    /// GPUI found the focused element missing from the frame just drawn.
    /// After a layout change left focus in place, the document takes it, as
    /// long as the Reader is still drawn (the element's nearest drawn ancestor
    /// is the Reader or inside it).
    pub(super) fn refocus_lost_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.refocus_if_lost) {
            return;
        }
        let reader_drawn = window.focus_lost_restore_target(cx).is_some_and(|target| {
            target == self.focus_handle || self.focus_handle.contains(&target, window)
        });
        if reader_drawn {
            self.focus_document(window, cx);
            cx.notify();
        }
    }

    /// Keyboard focus goes to the document as it is drawn (#2957): the editor
    /// while editing, the file viewer while a file preview replaces the note,
    /// otherwise the note. A file preview, the empty vault page, a timeline
    /// version or a tree Quick Look does not draw the note's TextView, so
    /// focusing it would leave key dispatch at the window root, outside every
    /// Reader context, until a click; the last three take the Reader's handle.
    pub(crate) fn focus_document(&self, window: &mut Window, cx: &mut App) {
        if self.main_area_without_document() {
            // Nothing that owns focus is drawn there; the Reader's handle
            // keeps its key contexts in the dispatch path.
            self.focus_handle.focus(window, cx);
        } else if let Some(editing) = &self.pane.editing {
            editing.input().focus_handle(cx).focus(window, cx);
        } else if let Some(preview) = &self.pane.file_preview {
            preview.focus(&self.focus_handle, window, cx);
        } else {
            self.pane
                .content
                .read(cx)
                .focus_handle()
                .clone()
                .focus(window, cx);
        }
    }

    /// `render_main` draws a tree Quick Look, a timeline version or the empty
    /// vault instead of the note, its editor or a file viewer.
    pub(super) fn main_area_without_document(&self) -> bool {
        self.tree_quick_look_shown() || self.timeline_preview_shown() || self.empty_vault_shown()
    }

    pub(super) fn empty_vault_shown(&self) -> bool {
        self.selected_file().is_empty()
            && !self
                .loading
                .as_ref()
                .is_some_and(|load| load.active && !load.published)
    }
}
