//! A bounded goal review; durable saves belong to the existing source editor.
use super::*;
use tessera_core::goal_criteria::{self, CriterionEdit};

struct Row {
    id: String,
    description: Entity<TextareaState>,
    requires_human: bool,
    saved: bool,
}
#[derive(Default)]
pub(super) struct GoalCriteriaUi {
    pub active: bool,
    goal: String,
    workspace: Option<Value>,
    base: Value,
    rows: Vec<Row>,
    frozen: bool,
    pub(super) readback: Option<Value>,
    error: Option<String>,
    destination: Option<PendingNavigation>,
    closing: bool,
}
impl BrainView {
    pub(super) fn open_goal_criteria(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.editor_closing() || self.capabilities["goal_criteria_edit"] != true {
            return;
        }
        if self.note_guard_navigation(PendingNavigation::GoalCriteria, cx) {
            return;
        }
        if self.source_dirty(cx) {
            self.pending_navigation = Some(PendingNavigation::GoalCriteria);
            cx.notify();
            return;
        }
        if !self.editor_can_begin_criteria() {
            self.error = Some("Recheck source recovery before opening a criteria review.".into());
            self.surface = Surface::Source;
            cx.notify();
            return;
        }
        let goal = self.goal_id();
        if goal.is_empty() {
            return;
        }
        let workspace = self.expected_workspace.clone();
        let endpoint = self.endpoint;
        self.busy = true;
        cx.spawn_in(window, async move |this, cx| {
            let owner = workspace.clone();
            let id = goal.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    rpc_guarded(
                        endpoint,
                        json!({"op":"goal_criteria_get","goal_id":id}),
                        owner.as_ref(),
                    )
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                if this.expected_workspace != workspace || this.goal_id() != goal || this.dirty(cx)
                {
                    this.error =
                        Some("Goal changed while loading criteria. Open the review again.".into());
                    cx.notify();
                    return;
                }
                match result.and_then(|data| {
                    if data["goal_id"] != goal {
                        return Err("Criteria reply belongs to another goal".into());
                    }
                    if data["editable"] != true {
                        return Err(text(&data["reason"]));
                    }
                    let base = serde_json::from_value(data["source"].clone())
                        .map_err(|_| "Invalid criteria source")?;
                    let rows = goal_criteria::inspect(&base, &goal)?;
                    Ok((data["source"].clone(), rows))
                }) {
                    Ok((base, rows)) => {
                        this.goal_criteria = GoalCriteriaUi {
                            active: true,
                            goal,
                            workspace,
                            base,
                            ..Default::default()
                        };
                        for row in rows {
                            this.criteria_add_row(row, true, window, cx);
                        }
                        this.error = None;
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn criteria_add_row(
        &mut self,
        row: CriterionEdit,
        saved: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let description = cx.new(|cx| {
            let mut input = TextareaState::new(window, cx).rows(2);
            input.set_value(row.description, window, cx);
            input
        });
        self.goal_criteria.rows.push(Row {
            id: row.id,
            description,
            requires_human: row.requires_human,
            saved,
        });
    }
    pub(super) fn save_goal_criteria(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.goal_criteria.active
            || self.goal_criteria.frozen
            || self.busy
            || self.editor_closing()
        {
            return;
        }
        let state = &self.goal_criteria;
        if self.expected_workspace != state.workspace || self.goal_id() != state.goal {
            self.goal_criteria.error =
                Some("This review belongs to another goal or workspace.".into());
            cx.notify();
            return;
        }
        let result = (|| {
            let base =
                serde_json::from_value(state.base.clone()).map_err(|_| "Invalid loaded source")?;
            let rows: Vec<_> = state
                .rows
                .iter()
                .map(|row| CriterionEdit {
                    id: row.id.clone(),
                    description: row.description.read(cx).value().to_string(),
                    requires_human: row.requires_human,
                })
                .collect();
            goal_criteria::transform(&base, &state.goal, &rows)
        })();
        match result {
            Ok(proposed) => {
                let mut request = source_write_reusing(&state.base, &proposed, None);
                request["op"] = json!("goal_criteria_write");
                request["goal_id"] = json!(state.goal);
                let base = state.base.clone();
                self.goal_criteria.frozen = true;
                self.goal_criteria.error = None;
                self.load_source(base, window, cx);
                self.source.reset(proposed, window, cx);
                self.editor_save(request, window, cx);
            }
            Err(error) => self.goal_criteria.error = Some(error),
        }
        cx.notify();
    }
    pub(super) fn criteria_guard_navigation(
        &mut self,
        destination: PendingNavigation,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.goal_criteria.active {
            return false;
        }
        self.goal_criteria.destination = Some(destination);
        self.goal_criteria.error = Some(
            "Save criteria, keep editing, or explicitly leave this review before continuing."
                .into(),
        );
        cx.notify();
        true
    }
    pub(super) fn criteria_request_close(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.goal_criteria.active {
            return false;
        }
        self.goal_criteria.closing = true;
        self.goal_criteria.error = Some(
            "This criteria review is still open. Keep editing or explicitly leave before closing."
                .into(),
        );
        cx.notify();
        true
    }
    pub(super) fn criteria_keep_editing(&mut self, cx: &mut Context<Self>) {
        self.goal_criteria.destination = None;
        self.goal_criteria.closing = false;
        if let Some(token) = self.app_quit_token {
            app_quit::queue_cancel(token, cx);
        }
        cx.notify();
    }
    fn leave_goal_criteria(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let state = std::mem::take(&mut self.goal_criteria);
        if state.frozen {
            self.surface = Surface::Source;
        } else {
            self.surface = Surface::Details;
        }
        if state.closing {
            self.note_finish_close(window, cx);
        } else if let Some(destination) = state.destination {
            self.pending_navigation = Some(destination);
            self.continue_navigation(window, cx);
        }
        cx.notify();
    }
    // A receipt proves a historical save. Read the canonical source separately,
    // including on recovery after a later external edit or stage transition.
    pub(super) fn criteria_saved_readback(
        &mut self,
        request: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.goal_criteria.readback = Some(request.clone());
        if self.source_dirty(cx)
            || self
                .source_snapshot
                .as_ref()
                .is_none_or(|s| s["content_base64"] != request["request"]["content_base64"])
        {
            self.error = Some("Earlier criteria Save is acknowledged. Preserve the newer local draft before reading its current source.".into());
            cx.notify();
            return;
        }
        let workspace = self.expected_workspace.clone();
        let owner = workspace.clone();
        let endpoint = self.endpoint;
        self.busy = true;
        self.source_loading = true;
        self.sync_source_policy(cx);
        cx.spawn_in(window, async move |this,cx| {
            let path = request["request"]["path"].clone();
            let expected = request.clone();
            let result = cx.background_executor().spawn(async move {
                rpc_guarded(endpoint,json!({"op":"source_read","path":path}),owner.as_ref())
            }).await;
            let _ = this.update_in(cx, |this,window,cx| {
                this.busy = false;
                this.source_loading = false;
                this.sync_source_policy(cx);
                if this.expected_workspace != workspace || this.source_snapshot.as_ref().is_none_or(|s| s["path"] != expected["request"]["path"]) {
                    this.error = Some("Earlier criteria were saved; the current view changed before readback.".into());
                    cx.notify(); return;
                }
                match result.and_then(|source| {
                    decode_source(&source)?;
                    if source["path"] != expected["request"]["path"] || source["brain_id"] != expected["request"]["brain_id"] {
                        return Err("Current source belongs to another owner".into());
                    }
                    Ok(source)
                }) {
                    Ok(source) => {
                        let changed = source["content_base64"] != expected["request"]["content_base64"];
                        this.load_source(source,window,cx);
                        let state = std::mem::take(&mut this.goal_criteria);
                        // A recovered save may belong to a different selected
                        // goal. Keep its current source visible in that case.
                        this.surface = if this.goal_id() == text(&expected["goal_id"]) { Surface::Details } else { Surface::Source };
                        this.notice = if changed { "Criteria save acknowledged. The source changed again afterwards; showing the current saved goal." } else { "Outcome criteria saved and read back." }.into();
                        if state.closing { this.note_finish_close(window,cx); }
                        else if let Some(destination) = state.destination {
                            this.pending_navigation = Some(destination);
                            this.continue_navigation(window,cx);
                        }
                        this.refresh(window,cx);
                    }
                    Err(error) => {
                        this.goal_criteria.error = Some(format!("Earlier Save is acknowledged, but current source is unavailable: {error}. Retry current-source readback."));
                        this.error = this.goal_criteria.error.clone();
                    }
                }
                cx.notify();
            });
        }).detach();
    }
    pub(super) fn goal_criteria_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let frozen = self.goal_criteria.frozen;
        let mut panel = v_flex().id("goal-criteria-review").debug_selector(|| "goal-criteria-review".into()).flex_1().min_h_0().h_full().overflow_y_scroll().p_6().gap_4()
            .child(div().text_2xl().font_weight(FontWeight::SEMIBOLD).child("Edit outcome criteria"))
            .child("Describe the outcomes you want to recognise. Saved criteria keep their identity and confirmation choice.");
        for (index, row) in self.goal_criteria.rows.iter().enumerate() {
            let mut item = v_flex()
                .gap_2()
                .child(div().text_sm().child(format!("Outcome {}", index + 1)))
                .child(Textarea::new(&row.description).disabled(frozen || self.busy))
                .child(
                    super::super::brand::control(("criteria-human", index), cx)
                        .ghost()
                        .label(if row.requires_human {
                            "Requires my confirmation: yes"
                        } else {
                            "Requires my confirmation: no"
                        })
                        .disabled(row.saved || frozen || self.busy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.goal_criteria.rows[index].requires_human =
                                !this.goal_criteria.rows[index].requires_human;
                            cx.notify();
                        })),
                );
            if !row.saved {
                item = item.child(
                    super::super::brand::control(("criteria-remove", index), cx)
                        .ghost()
                        .label("Remove new criterion")
                        .disabled(frozen || self.busy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.goal_criteria.rows.remove(index);
                            cx.notify();
                        })),
                );
            }
            panel = panel.child(item);
        }
        panel = panel.child(
            super::super::brand::control("criteria-add", cx)
                .ghost()
                .label("Add criterion")
                .disabled(frozen || self.busy || self.goal_criteria.rows.len() >= 64)
                .on_click(cx.listener(|this, _, window, cx| {
                    this.criteria_add_row(
                        CriterionEdit {
                            id: uuid(),
                            description: String::new(),
                            requires_human: true,
                        },
                        false,
                        window,
                        cx,
                    );
                    cx.notify();
                })),
        );
        if let Some(error) = &self.goal_criteria.error {
            panel = panel.child(div().text_sm().child(error.clone()));
        }
        if frozen {
            panel = panel.child(self.editor_panel(cx));
            if let Some(conflict) = self.source_conflict.clone() {
                panel = panel.child(div().font_weight(FontWeight::SEMIBOLD).child("Source changed — your proposed criteria are preserved"))
                    .child("Compare the original, current and proposed source. Leave for recovery to discard this proposal, then open a fresh criteria review.");
                for (label, side) in [
                    ("Original", "base"),
                    ("Current saved source", "current"),
                    ("Proposed source", "proposed"),
                ] {
                    panel = panel.child(div().text_sm().child(label)).child(
                        div().text_sm().whitespace_normal().child(
                            decode_source(&conflict[side])
                                .unwrap_or_else(|_| "Source is unavailable".into()),
                        ),
                    );
                }
            }
        }
        if self.goal_criteria.readback.is_some() {
            panel = panel.child(
                super::super::brand::control("criteria-readback", cx)
                    .label("Read current source")
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, window, cx| {
                        if let Some(request) = this.goal_criteria.readback.clone() {
                            this.criteria_saved_readback(request, window, cx);
                        }
                    })),
            );
        }
        panel
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        super::super::brand::control("criteria-save", cx)
                            .primary()
                            .label("Save criteria")
                            .disabled(frozen || self.busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.save_goal_criteria(window, cx)
                            })),
                    )
                    .child(
                        super::super::brand::control("criteria-cancel", cx)
                            .debug_selector(|| "criteria-cancel".into())
                            .ghost()
                            .label(if frozen {
                                "Continue in recovery"
                            } else {
                                "Cancel"
                            })
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.leave_goal_criteria(window, cx)
                            })),
                    )
                    .child(
                        super::super::brand::control("criteria-keep", cx)
                            .ghost()
                            .label("Keep editing")
                            .on_click(cx.listener(|this, _, _, cx| this.criteria_keep_editing(cx))),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[gpui::test]
    fn criteria_conflict_stays_in_viewport_and_scroll_reaches_recovery(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let bytes = "Retained original, current and proposed source detail.\n".repeat(150);
        let source = json!({"schema":SCHEMA,"brain_id":"fixture","path":"records/goal-fixture.md",
            "revision":"sha256:fixture","content_base64":STANDARD.encode(bytes.as_bytes())});
        let conflict = json!({"base":source,"current":source,"proposed":source});
        let retained = conflict.clone();
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            view.busy = true;
            view.goal_criteria.active = true;
            view.goal_criteria.frozen = true;
            view.source_conflict = Some(conflict);
            view
        });
        cx.run_until_parked();
        let panel = cx
            .debug_bounds("goal-criteria-review")
            .expect("review is rendered");
        assert!(
            panel.bottom() <= cx.update(|window, _| window.viewport_size().height),
            "conflict source must not grow the review beyond the viewport"
        );
        let initially_visible = cx
            .debug_bounds("criteria-cancel")
            .is_some_and(|button| button.top() >= panel.top() && button.bottom() <= panel.bottom());
        assert!(
            !initially_visible,
            "positive control: long conflict needs scrolling"
        );
        cx.simulate_event(ScrollWheelEvent {
            position: panel.center(),
            delta: ScrollDelta::Pixels(point(px(0.), px(-100_000.))),
            ..Default::default()
        });
        cx.run_until_parked();
        let recovery = cx
            .debug_bounds("criteria-cancel")
            .expect("recovery control is reachable");
        assert!(
            recovery.top() >= panel.top() && recovery.bottom() <= panel.bottom(),
            "scrolling must bring Continue in recovery fully into the viewport"
        );
        view.update(cx, |view, _| {
            assert_eq!(view.source_conflict, Some(retained))
        });
    }
}
