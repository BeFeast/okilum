use super::*;
use gpui_component::{
    input::{Input, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
    notification::Notification,
};
use okilum_core::now_actions::{self, Action, Receipt, Target};
use okilum_t3::projects::waiting::{Row, Source};
use std::collections::BTreeSet;

#[derive(Default)]
pub(super) struct Actions {
    area: Option<String>,
    pending_notes: BTreeSet<String>,
    pending_requests: BTreeSet<(Source, String)>,
    pub(super) answered: BTreeSet<String>,
    intentions: Vec<(String, String, String)>,
}

impl Actions {
    pub(super) fn accept_snapshot(&mut self) {
        self.answered.clear();
        self.intentions.clear();
    }
}

fn button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Button {
    Button::new(id).small().ghost().label(label)
}

fn input(
    root: &Path,
    key: &str,
    placeholder: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<InputState> {
    let key = SharedString::from(format!("now-inline:{}:{key}", root.display()));
    let placeholder = placeholder.into();
    window.use_keyed_state(key, cx, |window, cx| {
        InputState::new(window, cx).placeholder(placeholder)
    })
}

impl Reader {
    pub(super) fn now_case_form(
        &self,
        path: &str,
        item_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let target = self.now_feed.targets.cases.get(path)?;
        Some(self.now_note_form(target, Action::DecideCase, item_id, window, cx))
    }

    pub(super) fn now_intention_form(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let state = &self.now_feed.actions;
        let areas: Vec<_> = self
            .now_feed
            .targets
            .areas
            .keys()
            .map(|area| {
                (
                    area.clone(),
                    now_feed::area_label(area, &self.now_feed.areas),
                )
            })
            .collect();
        let mut form = v_flex().id("now-intention-form").px_3().items_start();
        if !areas.is_empty() {
            let reader = cx.weak_entity();
            let root = self.vault_root.clone();
            let label = state.area.as_ref().map_or_else(
                || "+ Intention".to_owned(),
                |area| {
                    format!(
                        "+ Intention · {}",
                        now_feed::area_label(area, &self.now_feed.areas)
                    )
                },
            );
            form = form.child(button("now-add-intention", label).dropdown_menu(
                move |mut menu, _, _| {
                    for (area, label) in &areas {
                        let (area, reader, root) = (area.clone(), reader.clone(), root.clone());
                        menu = menu.item(PopupMenuItem::new(label.clone()).on_click(
                            move |_, _, cx| {
                                let _ = reader.update(cx, |this, cx| {
                                    if this.vault_root == root {
                                        this.now_feed.actions.area = Some(area.clone());
                                        cx.notify();
                                    }
                                });
                            },
                        ));
                    }
                    menu
                },
            ));
        }
        if let Some(target) = state
            .area
            .as_ref()
            .and_then(|area| self.now_feed.targets.areas.get(area))
        {
            form = form.child(self.now_note_form(
                target,
                Action::AddIntention,
                target.path(),
                window,
                cx,
            ));
        }
        form.children(
            state
                .intentions
                .iter()
                .map(|(_, _, text)| div().child(text.clone())),
        )
        .into_any_element()
    }

    fn now_note_form(
        &self,
        target: &Target,
        action: Action,
        input_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (label, hint) = match action {
            Action::DecideCase => ("Submit", "Decision"),
            Action::AddIntention => ("Add", "New intention"),
        };
        let root = self.vault_root.clone();
        let path = target.path().to_owned();
        let busy = self.now_feed.actions.pending_notes.contains(&path);
        let input = input(&root, &format!("{path}:{input_id}"), hint, window, cx);
        let field = input.clone();
        let reader = cx.weak_entity();
        let target = target.clone();
        h_flex()
            .w_full()
            .min_w_0()
            .gap_1()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&input).small().disabled(busy)),
            )
            .child(
                button(
                    SharedString::from(format!("now-submit-{path}:{input_id}")),
                    label,
                )
                .disabled(busy)
                .on_click(move |_, window, cx| {
                    let _ = reader.update(cx, |this, cx| {
                        if this.vault_root == root && this.now_feed_ready() {
                            this.submit_now_note(target.clone(), action, field.clone(), window, cx);
                        }
                    });
                }),
            )
            .into_any_element()
    }

    fn submit_now_note(
        &mut self,
        target: Target,
        action: Action,
        input: Entity<InputState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.session_directory.clone() else {
            reader_toast::error("Editor recovery storage is unavailable", window, cx);
            return;
        };
        let path = target.path().to_owned();
        if !self.now_feed.actions.pending_notes.insert(path.clone()) {
            return;
        }
        cx.notify();
        let text = input.read(cx).value().to_string();
        let root = self.vault_root.clone();
        cx.spawn_in(window, async move |reader, cx| {
            let (job_root, drafts, reply) =
                (root.clone(), state.join("editor-drafts"), text.clone());
            let result = cx
                .background_executor()
                .spawn(async move {
                    now_actions::apply(
                        &job_root,
                        &drafts,
                        &target,
                        action,
                        &reply,
                        chrono::Utc::now(),
                    )
                })
                .await;
            let _ = reader.update_in_own_window(cx, |this, window, cx| {
                if this.vault_root != root || this.session_directory.as_ref() != Some(&state) {
                    return;
                }
                this.now_feed.actions.pending_notes.remove(&path);
                match result {
                    Ok(receipt) => {
                        input.update(cx, |input, cx| input.set_value("", window, cx));
                        let actions = &mut this.now_feed.actions;
                        let message = match action {
                            Action::DecideCase => {
                                actions.answered.insert(path.clone());
                                "Decision saved"
                            }
                            Action::AddIntention => {
                                actions.intentions.push((
                                    path.clone(),
                                    receipt.saved_revision().to_owned(),
                                    text.trim().to_owned(),
                                ));
                                "Intention added"
                            }
                        };
                        this.now_note_changed(path, cx);
                        let weak = cx.weak_entity();
                        reader_toast::push(
                            Notification::new()
                                .message(message)
                                .content(move |_, _, _| {
                                    let (weak, receipt, root) =
                                        (weak.clone(), receipt.clone(), root.clone());
                                    button("now-action-undo", "Undo")
                                        .on_click(move |_, window, cx| {
                                            let _ = weak.update(cx, |this, cx| {
                                                if this.vault_root == root {
                                                    this.undo_now_note(receipt.clone(), window, cx);
                                                }
                                            });
                                        })
                                        .into_any_element()
                                }),
                            Some(Duration::from_secs(8)),
                            window,
                            cx,
                        );
                    }
                    Err(error) => reader_toast::error(error.to_string(), window, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn now_note_changed(&mut self, path: String, cx: &mut Context<Self>) {
        self.queue_vault_mutation(
            okilum_core::Changes {
                changed: [path].into_iter().collect(),
                ..Default::default()
            },
            cx,
        );
        self.refresh_now_feed(cx);
    }

    fn undo_now_note(&mut self, receipt: Receipt, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.session_directory.clone() else {
            return;
        };
        let path = receipt.path().to_owned();
        if !self.now_feed.actions.pending_notes.insert(path.clone()) {
            return;
        }
        cx.notify();
        let root = self.vault_root.clone();
        let revision = receipt.saved_revision().to_owned();
        cx.spawn_in(window, async move |reader, cx| {
            let job_root = root.clone();
            let result = cx
                .background_executor()
                .spawn(async move { receipt.undo(&job_root, &state.join("editor-drafts")) })
                .await;
            let _ = reader.update_in_own_window(cx, |this, window, cx| {
                if this.vault_root != root {
                    return;
                }
                let actions = &mut this.now_feed.actions;
                actions.pending_notes.remove(&path);
                match result {
                    Ok(()) => {
                        actions.answered.remove(&path);
                        actions
                            .intentions
                            .retain(|(_, saved, _)| saved != &revision);
                        this.now_note_changed(path, cx);
                        reader_toast::transient("Change undone", window, cx);
                    }
                    Err(error) => reader_toast::error(format!("Cannot undo: {error}"), window, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn now_live_form(
        &self,
        row: Row,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let questions = row.inline_questions()?;
        let busy = self
            .now_feed
            .actions
            .pending_requests
            .contains(&(row.source.clone(), row.request_id.clone()));
        let reader = cx.weak_entity();
        let form_id = SharedString::from(format!(
            "now-live-form-{}",
            reader_now_waiting::feed_id(&row)
        ));
        let mut form = v_flex().id(form_id).w_full().gap_1();
        let mut reply = h_flex()
            .w_full()
            .gap_1()
            .when(questions.len() > 1, |row| row.flex_col());
        let mut fields = Vec::new();
        for (id, question) in &questions {
            if question.allow_custom_answer != Some(false) {
                let key = format!("{:?}:{}:{id}", row.source, row.request_id);
                let hint = if questions.len() == 1 {
                    "Reply".into()
                } else {
                    question.question.clone()
                };
                let field = input(&self.vault_root, &key, hint, window, cx);
                let field_view = Input::new(&field).small().disabled(busy);
                reply = reply.child(field_view.w_full().flex_1().min_w_0());
                fields.push(((*id).to_owned(), field));
            }
        }
        if let [(id, question)] = questions.as_slice() {
            for (index, option) in question.options.iter().enumerate() {
                let option_button = button(("now-quick-answer", index), option.label.clone())
                    .when_some(option.description.clone(), Button::tooltip);
                let (reader, row) = (reader.clone(), row.clone());
                let value = option.value.as_ref().unwrap_or(&option.label);
                let answer = if question.multi_select {
                    serde_json::json!([value])
                } else {
                    serde_json::json!(value)
                };
                let answers = serde_json::json!({ (*id): answer });
                form = form.child(option_button.disabled(busy).on_click(move |_, window, cx| {
                    let _ = reader.update(cx, |this, cx| {
                        this.submit_now_live(&row, answers.clone(), window, cx)
                    });
                }));
            }
        }
        if !fields.is_empty() {
            let submit = button("now-live-submit", "Submit").disabled(busy).on_click(
                move |_, window, cx| {
                    let answers: serde_json::Map<_, _> = fields
                        .iter()
                        .map(|(id, field)| {
                            (id.clone(), field.read(cx).value().trim().to_owned().into())
                        })
                        .collect();
                    if answers.values().any(|value| value.as_str() == Some("")) {
                        reader_toast::error("Enter an answer for every question", window, cx);
                        return;
                    }
                    let _ = reader.update(cx, |this, cx| {
                        this.submit_now_live(&row, answers.into(), window, cx)
                    });
                },
            );
            form = form.child(reply.child(submit));
        }
        Some(form.into_any_element())
    }

    fn submit_now_live(
        &mut self,
        row: &Row,
        answers: serde_json::Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.now_live_current(row, cx) {
            reader_toast::error("The manager question is no longer current", window, cx);
            return;
        }
        let Some(gateway) = self.waiting.gateways.get(&row.source.environment).cloned() else {
            reader_toast::error("Connect to T3 to answer the question", window, cx);
            return;
        };
        let key = (row.source.clone(), row.request_id.clone());
        if !self.now_feed.actions.pending_requests.insert(key.clone()) {
            return;
        }
        cx.notify();
        let root = self.vault_root.clone();
        let permit = okilum_t3::NetworkAccess::new(true);
        let revoke = permit.clone();
        let expected = row.clone();
        let expected_root = root.clone();
        let subscription = cx.observe_self(move |this, cx| {
            if this.vault_root != expected_root || !this.now_live_current(&expected, cx) {
                revoke.revoke();
            }
        });
        let revoke = permit.clone();
        let release = cx.on_release(move |_, _| revoke.revoke());
        cx.spawn_in(window, async move |reader, cx| {
            let (source, request) = key.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    anyhow::ensure!(
                        permit.allowed(),
                        "The manager question is no longer current"
                    );
                    let _scope = permit.enter();
                    gateway.respond_pending_request(&source.thread_id, &request, &answers)?;
                    Ok::<_, anyhow::Error>(())
                })
                .await;
            drop(subscription);
            drop(release);
            let _ = reader.update_in_own_window(cx, |this, window, cx| {
                if this.vault_root != root {
                    return;
                }
                this.now_feed.actions.pending_requests.remove(&key);
                match result {
                    Ok(()) => {
                        this.confirm_waiting_answer(key);
                        this.refresh_waiting(window, cx);
                    }
                    Err(error) => reader_toast::error(format!("Reply failed: {error}"), window, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn now_live_current(&self, row: &Row, cx: &App) -> bool {
        self.now_feed_ready()
            && self.sidebar.left_mode == reader_sidebar::LeftMode::Now
            && self.sidebar.now_enabled()
            && reader_features::enabled(reader_features::Feature::Agents, cx)
            && okilum_t3::projects::waiting::sources(&self.project_navigation.discovery.projects)
                .into_iter()
                .any(|(_, source)| source.as_ref() == Ok(&row.source))
            && self.waiting.view().rows.iter().any(|current| {
                !current.stale
                    && current.source == row.source
                    && current.request_id == row.request_id
                    && current.questions == row.questions
            })
    }
}
