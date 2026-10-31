use super::*;
use okilum_t3::launch::{Draft, LaunchContext, Request};

pub(super) struct Form {
    pub(super) open: bool,
    role: Entity<InputState>,
    brief: Entity<InputState>,
    worktree: Entity<InputState>,
    branch: Entity<InputState>,
    context: Option<LaunchContext>,
    model: Option<okilum_t3::ModelSelection>,
    requested: Option<String>,
    error: Option<String>,
    _inputs: Vec<Subscription>,
}

impl Form {
    pub(super) fn new(window: &mut Window, cx: &mut Context<ProjectView>) -> Self {
        let mut input = |placeholder: &'static str| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let role = input("Executor role");
        let brief = input("Brief file relative to this vault area, e.g. specs/ui.md");
        let worktree = input("Absolute worktree path on the T3 server");
        let branch = input("Worktree branch");
        let subscriptions = [&role, &brief, &worktree, &branch]
            .into_iter()
            .map(|input| cx.subscribe(input, |_, _, _: &InputEvent, cx| cx.notify()))
            .collect();
        Self {
            open: false,
            role,
            brief,
            worktree,
            branch,
            context: None,
            model: None,
            requested: None,
            error: None,
            _inputs: subscriptions,
        }
    }
}

impl ProjectView {
    fn unresolved_launch(&self) -> bool {
        self.launch.requested.as_ref().is_some_and(|id| {
            self.records
                .iter()
                .find(|r| &r.message.client_request_id == id)
                .is_none_or(|r| r.receipt.is_none())
        }) || self
            .records
            .iter()
            .any(|r| r.receipt.is_none() && r.message.text.starts_with(okilum_t3::launch::PREFIX))
    }

    pub(super) fn open_launch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.model_busy || !self.can_send() {
            return;
        }
        let Some(binding) = self.binding.clone() else {
            return;
        };
        let Some(gateway) = self.gateway.clone() else {
            return;
        };
        self.launch.open = true;
        self.launch.error = None;
        self.busy = true;
        let generation = self.generation;
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { LaunchContext::load(gateway.as_ref(), &binding) })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                this.busy = false;
                if this.generation == generation {
                    match result {
                        Ok(context) => {
                            this.launch.context = Some(context);
                            this.launch.error = None;
                        }
                        Err(error) => {
                            this.launch.context = None;
                            this.launch.error = Some(format!("{error:#}"));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn submit_launch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy
            || self.model_busy
            || !self.can_send()
            || self.unresolved_launch()
            || self.launch.requested.is_some()
        {
            return;
        }
        let Some(area) = self.area.clone() else {
            self.launch.error = Some("This Project has no vault area for its brief.".into());
            cx.notify();
            return;
        };
        let Some(binding) = self.binding.clone() else {
            return;
        };
        let Some(gateway) = self.gateway.clone() else {
            return;
        };
        let Some(journal) = self.journal() else {
            self.launch.error =
                Some("Durable application state is unavailable; nothing was requested.".into());
            cx.notify();
            return;
        };
        let draft = Draft {
            role: self.launch.role.read(cx).value().to_string(),
            brief_path: self.launch.brief.read(cx).value().to_string(),
            worktree_path: self.launch.worktree.read(cx).value().to_string(),
            branch: self.launch.branch.read(cx).value().to_string(),
            model_override: self.launch.model.clone(),
        };
        self.busy = true;
        self.launch.error = None;
        let generation = self.generation;
        cx.spawn_in(window, async move |this, cx| {
            let (result, records, retained) = cx
                .background_executor()
                .spawn(async move {
                    let mut retained = None;
                    let result = (|| {
                        // Refresh the actual manager/project/catalog before retaining
                        // this intent; a stale open form must not change its target.
                        let context = LaunchContext::load(gateway.as_ref(), &binding)?;
                        let request = Request::prepare(&area, &binding, &context, &draft)?;
                        let record = journal.prepare(&request.message()?)?;
                        retained = Some(record.message.client_request_id.clone());
                        journal.send(gateway.as_ref(), &record)?;
                        Ok::<_, anyhow::Error>(())
                    })();
                    (result, journal.records(), retained)
                })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                this.busy = false;
                if this.generation == generation {
                    if retained.is_some() {
                        this.launch.requested = retained;
                    }
                    match records {
                        Ok(records) => this.records = records,
                        Err(error) => this.launch.error = Some(format!("{error:#}")),
                    }
                    if let Err(error) = result {
                        this.launch.error = Some(format!("{error:#}"));
                    }
                }
                // All form fields and the independent chat draft are retained.
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn render_launch(&self, cx: &mut Context<Self>) -> AnyElement {
        let unresolved = self.unresolved_launch();
        let locked = self.busy || self.model_busy || self.launch.requested.is_some() || unresolved;
        let mut panel = v_flex()
            .id("project-launch-panel")
            .debug_selector(|| "project-launch-panel".into())
            .max_h(px(320.))
            .overflow_y_scroll()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child("Request one executor through the manager")
                    .child(
                        Button::new("project-launch-close")
                            .ghost()
                            .label("Close")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.launch.open = false;
                                cx.notify();
                            })),
                    ),
            )
            .child(Input::new(&self.launch.role).disabled(locked))
            .child(Input::new(&self.launch.brief).disabled(locked))
            .child(Input::new(&self.launch.worktree).disabled(locked))
            .child(Input::new(&self.launch.branch).disabled(locked));
        if let Some(context) = &self.launch.context {
            panel = panel.child(format!(
                "Repo project: {} · {}",
                context.project.title, context.project.workspace_root
            ));
            let default = context
                .project
                .default_model_selection
                .as_ref()
                .map(|m| format!("{} / {}", m.instance_id, m.model))
                .unwrap_or_else(|| "not configured; choose an override".into());
            panel = panel.child(
                Button::new("project-launch-default-model")
                    .ghost()
                    .label(format!("Project default: {default}"))
                    .selected(self.launch.model.is_none())
                    .disabled(locked)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.launch.model = None;
                        cx.notify();
                    })),
            );
            let mut choices = v_flex()
                .id("project-launch-models")
                .max_h(px(90.))
                .overflow_y_scroll();
            let mut index = 0usize;
            for provider in &context.catalog.providers {
                for model in &provider.models {
                    let selection = okilum_t3::ModelSelection {
                        instance_id: provider.provider_instance_id.clone(),
                        model: model.id.clone(),
                        options: vec![],
                    };
                    let selected = self.launch.model.as_ref() == Some(&selection);
                    choices = choices.child(
                        Button::new(("project-launch-model", index))
                            .ghost()
                            .label(format!("{} / {}", provider.provider_instance_id, model.id))
                            .selected(selected)
                            .disabled(locked)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.launch.model = Some(selection.clone());
                                cx.notify();
                            })),
                    );
                    index += 1;
                }
            }
            panel = panel.child(choices);
        }
        if let Some(error) = &self.launch.error {
            panel = panel.child(
                div()
                    .debug_selector(|| "project-launch-error".into())
                    .text_color(cx.theme().danger)
                    .child(error.clone()),
            );
        }
        if unresolved {
            panel = panel.child("Launch delivery is unconfirmed. Use Retry for the retained request in Manager; do not request another executor.");
        } else if self.launch.requested.is_some() {
            panel = panel.child(div().debug_selector(|| "project-launch-requested".into()).child(
                "Launch requested: message delivered to the manager conversation. Manager acceptance and T3 launch are not confirmed. Follow the manager chat and registered executor list."));
        }
        panel
            .child(okilum_t3::models::SCHEDULED_TASK_LIMIT)
            .child(
                Button::new("project-launch-submit")
                    .debug_selector(|| "project-launch-submit".into())
                    .ghost()
                    .label("Request executor")
                    .disabled(locked || !self.can_send() || self.launch.context.is_none())
                    .on_click(cx.listener(|this, _, window, cx| this.submit_launch(window, cx))),
            )
            .when(self.launch.requested.is_some(), |panel| {
                panel.child(
                    Button::new("project-launch-new")
                        .ghost()
                        .label("Start a separate executor request")
                        .disabled(self.busy || unresolved)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.launch.requested = None;
                            cx.notify();
                        })),
                )
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[gpui::test]
    fn launch_validation_and_lost_receipt_preserve_form_and_retry_one_retained_intent(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::test_init);
        let files = tempfile::tempdir().unwrap();
        let area = files.path().join("area");
        std::fs::create_dir_all(area.join("ops")).unwrap();
        std::fs::write(
            area.join("brief.md"),
            "# UI brief\nRead-only fixture request\n",
        )
        .unwrap();
        let gateway = Arc::new(fixture::Fake {
            fail_first: true,
            ..Default::default()
        });
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                let mut view =
                    fixture::view(files.path().join("state"), gateway.clone(), window, cx);
                view.area = Some(area.clone());
                std::fs::write(
                    area.join("ops/project.yaml"),
                    serde_yaml::to_string(&serde_json::json!({"t3":view.binding})).unwrap(),
                )
                .unwrap();
                for (input, value) in [
                    (&view.launch.role, "UI"),
                    (&view.launch.brief, "missing.md"),
                    (&view.launch.worktree, "/server/worktrees/ui"),
                    (&view.launch.branch, "feature/ui"),
                ] {
                    input.update(cx, |input, cx| input.set_value(value, window, cx));
                }
                view.compose.update(cx, |input, cx| {
                    input.set_value("Independent chat draft", window, cx)
                });
                view
            });
            entity = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = entity.unwrap();
        let open = visual.debug_bounds("project-connection-menu").unwrap();
        visual.simulate_click(open.center(), Modifiers::default());
        visual.run_until_parked();
        visual.simulate_keystrokes("down down down enter");
        visual.run_until_parked();
        view.read_with(visual, |view, _| assert!(view.launch.open));
        visual.update(|window, cx| view.update(cx, |v, cx| v.submit_launch(window, cx)));
        visual.run_until_parked();
        view.read_with(visual, |v, cx| {
            assert!(v.launch.error.as_ref().unwrap().contains("vault brief"));
            assert_eq!(v.launch.brief.read(cx).value().as_ref(), "missing.md");
            assert!(v.records.is_empty());
        });
        assert!(
            gateway.sends.lock().unwrap().is_empty(),
            "invalid brief must not dispatch"
        );
        visual.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.launch
                    .brief
                    .update(cx, |input, cx| input.set_value("brief.md", window, cx));
                v.submit_launch(window, cx);
            })
        });
        visual.run_until_parked();
        view.read_with(visual, |v, cx| {
            assert!(v.unresolved_launch());
            assert_eq!(v.records.len(), 1);
            assert_eq!(
                v.compose.read(cx).value().as_ref(),
                "Independent chat draft"
            );
            assert_eq!(v.launch.role.read(cx).value().as_ref(), "UI");
            let request = Request::from_message(&v.records[0].message.text)
                .unwrap()
                .unwrap();
            assert_eq!(
                request.brief_source,
                "# UI brief\nRead-only fixture request\n"
            );
            assert_eq!(request.executor_project_id, "fixture-executors");
        });
        visual.update(|window, cx| view.update(cx, |v, cx| v.submit_launch(window, cx)));
        visual.run_until_parked();
        assert_eq!(
            gateway.sends.lock().unwrap().len(),
            1,
            "unconfirmed delivery blocks a second launch intent"
        );
        visual.update(|window, cx| {
            view.update(cx, |v, cx| v.send(Some(v.records[0].clone()), window, cx))
        });
        visual.run_until_parked();
        view.read_with(visual, |v, cx| {
            assert!(!v.unresolved_launch());
            assert!(v.records[0].receipt.is_some());
            assert_eq!(
                v.compose.read(cx).value().as_ref(),
                "Independent chat draft"
            );
        });
        let sends = gateway.sends.lock().unwrap();
        assert_eq!(sends.len(), 2);
        assert_eq!(
            sends[0], sends[1],
            "retry preserves both operation payload and clientRequestId"
        );
    }
}
