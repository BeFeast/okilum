//! Reader-coupled glue and Reader-integration tests for `project::board`.
use super::*;
#[cfg(test)]
use okilum_shell_kit::clock::now;
pub(crate) use okilum_shell_project::project::board::Action;
pub(crate) use okilum_shell_project::project::board::*;
#[cfg(test)]
use okilum_t3::ServerIdentity;

#[cfg(test)]
pub(crate) mod list {
    #[cfg(test)]
    use super::*;
    pub(crate) use okilum_shell_project::project::board::list::*;
    #[cfg(test)]
    mod tests;
}

#[cfg(test)]
mod tests;

#[cfg(all(test, native_manager))]
mod discuss_tests;

impl Reader {
    fn return_to_project_board(&mut self, cx: &mut Context<Self>) {
        if !self.pane.current_rel.is_empty() {
            let position = self.retained_document_position(cx);
            self.pane.navigation.places.insert(
                (self.vault_root.clone(), self.pane.current_rel.clone()),
                position,
            );
        }
        self.cancel_pending_landing();
        self.pane.navigation.preparation_generation =
            self.pane.navigation.preparation_generation.wrapping_add(1);
        self.project_navigation.board_visible = true;
        cx.notify();
    }

    pub(crate) fn ensure_project_board(
        &mut self,
        area: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_project_board_using(area, okilum_t3::board::live::reader(), window, cx);
    }

    fn ensure_project_board_using(
        &mut self,
        area: PathBuf,
        read_issues: impl Fn(&okilum_t3::board::Binding) -> Result<Vec<okilum_t3::board::Issue>, String>
            + Send
            + Sync
            + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.project_navigation.board_visible = true;
        self.ensure_project_records(&area, window, cx);
        if self.project_navigation.boards.contains_key(&area) {
            return;
        }
        let board = cx.new(|cx| Board::with_reader(area.clone(), read_issues, window, cx));
        let owner = area.clone();
        cx.subscribe_in(&board, window, move |this, board, action: &Action, window, cx| {
            if this.project_navigation.active.as_ref().map(|p| &p.folder) != Some(&owner) { return; }
            match action {
                Action::Peek => {
                    this.panels.open(reader_layout::Panel::Backlinks);
                    cx.notify();
                }
                Action::Note(note) => {
                    let target = owner.join(note);
                    let opened = (|| -> anyhow::Result<String> {
                        anyhow::ensure!(okilum_t3::board::relative_note(note), "Invalid Board note path");
                        let canonical = target.canonicalize()?;
                        anyhow::ensure!(canonical.starts_with(owner.canonicalize()?), "Board note escapes the project");
                        let relative = canonical.strip_prefix(this.vault_root.canonicalize()?)?;
                        Ok(okilum_core::vault::note_path(relative))
                    })();
                    match opened {
                        Ok(relative) => this.open_note(&relative, None, window, cx),
                        Err(error) => { this.link_notice = Some(format!("Board note unavailable: {error:#}").into()); cx.notify(); }
                    }
                }
                Action::Discuss(text) | Action::RecordDiscuss(text) => {
                    board.update(cx, |board,cx| { board.peek.manager_tab = true; cx.notify(); });
                    let result = this.project.clone().ok_or(okilum_t3::draft_handoff::Refusal::Unavailable)
                        .and_then(|manager| {
                            if manager.read(cx).area.as_deref() != Some(owner.as_path()) {
                                return Err(okilum_t3::draft_handoff::Refusal::WrongTarget);
                            }
                            // Discuss explicitly opens the existing panel first. It never
                            // selects/creates a thread or inserts into a hidden fallback.
                            this.panels.open(reader_layout::Panel::Backlinks);
                            let available = if this.body_bounds.size.width > px(0.) {
                                f32::from((this.body_bounds.size.width + window.viewport_size().width
                                    - this.body_viewport_width).max(px(0.)))
                            } else { f32::from(window.viewport_size().width) };
                            let visible = this.project_open && this.project_navigation.active.is_some()
                                && this.workspace_panels().visible(reader_layout::Panel::Backlinks, available);
                            manager.update(cx, |manager, cx| {
                                manager.select_section(ProjectSection::Manager, cx);
                                #[cfg(native_manager)]
                                manager.manager_surface_visible(visible, cx);
                                if matches!(action, Action::RecordDiscuss(_)) {
                                    manager.append_record_context(&owner, text.clone(), visible, window, cx)
                                } else {
                                    manager.append_board_context(&owner, text.clone(), window, cx)
                                }
                            })
                        });
                    let notice = match result {
                        Ok(()) => {
                            "Context appended to the existing Manager draft. Review and send when ready; Undo removes the block."
                        }
                        Err(okilum_t3::draft_handoff::Refusal::StaleAcknowledgement) =>
                            "Manager target changed. Check its draft before trying again.",
                        Err(_) => "Manager draft unavailable. Open the Manager and try again.",
                    };
                    board.update(cx, |board, cx| {
                        board.notice = Some(notice.into());
                        cx.notify();
                    });
                    if let Some(records) = this.project_navigation.records.get(&owner) {
                        records.update(cx, |records, cx| {
                            records.notice = Some(notice.into());
                            cx.notify();
                        });
                    }
                }
            }
        }).detach();
        cx.observe(&board, |this, _, cx| this.project_board_changed(cx))
            .detach();
        self.project_navigation.boards.insert(area, board);
    }

    pub(in crate::project) fn project_board_changed(&mut self, cx: &mut Context<Self>) {
        // The center and issue dock consume Board publications. The fixed
        // Reader header has no new source data when Board geometry changes.
        #[cfg(target_os = "linux")]
        self.layout_signal.update(cx, |_, cx| cx.notify());
        #[cfg(not(target_os = "linux"))]
        cx.notify();
    }

    pub(crate) fn connect_project_board(
        &self,
        manager: Entity<ProjectView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(board) = self
            .project_navigation
            .active
            .as_ref()
            .and_then(|p| self.project_navigation.boards.get(&p.folder))
        {
            board.update(cx, |board, cx| board.set_manager(manager, window, cx));
        }
    }

    pub(crate) fn sync_project_board_visibility(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let active = self.project_navigation.active.as_ref().map(|p| &p.folder);
        for (area, board) in &self.project_navigation.boards {
            let visible = active == Some(area)
                && self.project_navigation.board_visible
                && self.project_navigation.section == reader_project_navigation::Section::Reader;
            let records = self.project_navigation.records.get(area);
            let record_tab = records.is_some_and(|records| records.read(cx).kind.is_some());
            board.update(cx, |board, cx| {
                board.set_visible(visible && !record_tab, window, cx)
            });
            if let Some(records) = records {
                records.update(cx, |records, cx| {
                    records.set_visible(visible && record_tab, cx)
                });
            }
        }
    }

    /// The document surface is built from this small frame; board and
    /// return-bar builders keep their temporaries in separate frames (#2825).
    pub(crate) fn render_project_center(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let board = self
            .project_navigation
            .active
            .as_ref()
            .and_then(|p| self.project_navigation.boards.get(&p.folder));
        let project_reader = self.project_navigation.section
            == reader_project_navigation::Section::Reader
            && board.is_some();
        if project_reader && self.project_navigation.board_visible {
            return self.render_project_board(board.unwrap(), cx);
        }
        let surface = self.render_document_surface(window, cx);
        if project_reader {
            return self.render_project_return(surface, cx);
        }
        surface
    }

    #[inline(never)]
    fn render_project_board(&self, board: &Entity<Board>, cx: &mut Context<Self>) -> AnyElement {
        let resizing = self.resizing_panel.is_some();
        board.update(cx, |board, cx| {
            if board.resizing != resizing {
                board.resizing = resizing;
                cx.notify();
            }
        });
        let records = self
            .project_navigation
            .active
            .as_ref()
            .and_then(|p| self.project_navigation.records.get(&p.folder));
        let record_tab = records.is_some_and(|records| records.read(cx).kind.is_some());
        let content = if record_tab {
            records.unwrap().clone().into_any_element()
        } else {
            board
                .clone()
                .cached(StyleRefinement::default().size_full())
                .into_any_element()
        };
        v_flex()
            .size_full()
            .min_h_0()
            .min_w_0()
            .child(self.render_project_tabs(cx))
            .child(div().flex_1().min_h_0().min_w_0().child(content))
            .into_any_element()
    }

    #[inline(never)]
    fn render_project_return(&self, surface: AnyElement, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .size_full()
            .min_h_0()
            .child(
                h_flex().px_3().py_1().child(
                    Button::new("project-return-board")
                        .debug_selector(|| "project-return-board".into())
                        .ghost()
                        .label("← Project views")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.return_to_project_board(cx);
                        })),
                ),
            )
            .child(div().flex_1().min_h_0().child(surface))
            .into_any_element()
    }

    pub(crate) fn project_issue_peek_visible(&self, cx: &App) -> bool {
        self.project_navigation
            .active
            .as_ref()
            .and_then(|p| self.project_navigation.boards.get(&p.folder))
            .is_some_and(|board| {
                let board = board.read(cx);
                board.peek.open && !board.peek.manager_tab
            })
    }

    pub(crate) fn wrap_project_peek(
        &self,
        manager: AnyElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if let Some(board) = self
            .project_navigation
            .active
            .as_ref()
            .and_then(|p| self.project_navigation.boards.get(&p.folder))
        {
            board.update(cx, |board, cx| board.render_peek_dock(manager, cx))
        } else {
            manager
        }
    }
}
