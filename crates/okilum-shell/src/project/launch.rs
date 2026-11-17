use super::*;
use okilum_t3::launch::{
    progress::{Observation, Tracker},
    Draft, LaunchContext, Request,
};

pub(super) struct Form {
    pub(super) open: bool,
    role: Entity<InputState>,
    brief: Entity<InputState>,
    worktree: Entity<InputState>,
    branch: Entity<InputState>,
    context: Option<LaunchContext>,
    model: Option<okilum_t3::ModelSelection>,
    requested: Option<String>,
    /// Progress of the delivered request named by `requested`, read from T3.
    tracker: Option<Tracker>,
    /// The request whose status loop is running; at most one loop runs.
    polling: Option<String>,
    status_error: Option<String>,
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
            tracker: None,
            polling: None,
            status_error: None,
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
        self.poll_launch(window, cx);
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
                        // Threads that already exist are never candidates for this
                        // launch. Without the listing, new threads are inspected.
                        let baseline = gateway.list_threads(&request.executor_project_id).ok();
                        let record = journal.prepare(&request.message()?)?;
                        retained = Some(Tracker::new(
                            &request,
                            record.message.client_request_id.clone(),
                            record.message.thread_id.clone(),
                            baseline.as_deref(),
                        ));
                        journal.send(gateway.as_ref(), &record)?;
                        Ok::<_, anyhow::Error>(())
                    })();
                    (result, journal.records(), retained)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                if this.generation == generation {
                    if let Some(tracker) = retained {
                        this.launch.requested = Some(tracker.client_request_id.clone());
                        this.launch.tracker = Some(tracker);
                        this.launch.status_error = None;
                        this.poll_launch(window, cx);
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

    /// Follows the delivered request in T3 until its final outcome. Reads run
    /// on the background executor; an observation of another request or of an
    /// older tracker revision never replaces the current state.
    pub(super) fn poll_launch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tracker) = self.launch.tracker.as_ref() else {
            return;
        };
        if tracker.progress.finished()
            || self.launch.polling.as_deref() == Some(tracker.request_id.as_str())
        {
            return;
        }
        // A loop of an older request sees this and ends at its next turn.
        let request_id = tracker.request_id.clone();
        self.launch.polling = Some(request_id.clone());
        let generation = self.generation;
        cx.spawn_in(window, async move |this, cx| loop {
            let job = this
                .update_in(cx, |this, window, _| {
                    let own = this.launch.polling.as_deref() == Some(request_id.as_str());
                    let job = this
                        .launch
                        .tracker
                        .as_ref()
                        .filter(|tracker| {
                            own && this.generation == generation
                                && tracker.request_id == request_id
                                && !tracker.progress.finished()
                                && this.chat.as_ref().map(|chat| chat.thread_id.as_str())
                                    == Some(tracker.manager_thread_id())
                        })
                        .and_then(|tracker| {
                            Some((
                                tracker.clone(),
                                this.gateway.clone()?,
                                this.journal()?,
                                window.is_window_active(),
                            ))
                        });
                    if job.is_none() && own {
                        this.launch.polling = None;
                    }
                    job
                })
                .ok()
                .flatten();
            let Some((tracker, gateway, journal, focused)) = job else {
                break;
            };
            let result = cx
                .background_executor()
                .spawn(async move {
                    let receipt = journal
                        .records()?
                        .into_iter()
                        .find(|r| r.message.client_request_id == tracker.client_request_id)
                        .and_then(|r| r.receipt);
                    let observation: Option<Observation> =
                        tracker.observe(gateway.as_ref(), receipt.as_ref())?;
                    Ok::<_, anyhow::Error>((receipt, observation))
                })
                .await;
            let delay = this
                .update_in(cx, |this, _, cx| {
                    let current = this
                        .launch
                        .tracker
                        .as_mut()
                        .filter(|tracker| tracker.request_id == request_id)?;
                    let mut changed = false;
                    let delay = match result {
                        Ok((receipt, observation)) => {
                            if let Some(receipt) = &receipt {
                                changed |= current.record_receipt(receipt);
                            }
                            if let Some(observation) = observation {
                                changed |= current.apply(observation);
                            }
                            changed |= this.launch.status_error.take().is_some();
                            if focused {
                                2_000
                            } else {
                                10_000
                            }
                        }
                        Err(error) => {
                            let error = Some(format!("{error:#}"));
                            changed |= this.launch.status_error != error;
                            this.launch.status_error = error;
                            15_000
                        }
                    };
                    if changed {
                        cx.notify();
                    }
                    Some(delay)
                })
                .ok()
                .flatten();
            if let Some(delay) = delay {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(delay))
                    .await;
            }
        })
        .detach();
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
        } else if let Some(tracker) = self
            .launch
            .tracker
            .as_ref()
            .filter(|t| self.launch.requested.as_ref() == Some(&t.client_request_id))
        {
            panel = panel.child(self.render_launch_status(tracker, cx));
        } else if self.launch.requested.is_some() {
            panel = panel.child(div().debug_selector(|| "project-launch-requested".into()).child(
                "Launch requested: message delivered to the manager conversation. Follow the manager chat and registered executor list."));
        }
        panel
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
                            this.launch.tracker = None;
                            this.launch.status_error = None;
                            cx.notify();
                        })),
                )
            })
            .into_any_element()
    }

    fn render_launch_status(&self, tracker: &Tracker, cx: &mut Context<Self>) -> AnyElement {
        let progress = &tracker.progress;
        let mut status = v_flex()
            .gap_1()
            .debug_selector(|| "project-launch-status".into())
            .child(
                div()
                    .debug_selector(|| "project-launch-status-line".into())
                    .child(format!("Launch status: {}", progress.line())),
            );
        if let Some(executor) = &progress.executor {
            let mut row =
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_color(brand::palette(cx).text_muted)
                            .child(format!(
                                "Executor: {} · T3 status {} · {}",
                                executor.title, executor.status, executor.thread_id
                            )),
                    );
            if let Some(identity) = &self.identity {
                let url = identity.thread_url(&executor.thread_id);
                row = row.child(
                    Button::new("project-launch-open-t3")
                        .ghost()
                        .small()
                        .label("Open in T3")
                        .icon(IconName::ExternalLink)
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                );
            }
            status = status.child(row);
        } else if !progress.finished() {
            status = status.child(
                div()
                    .text_color(brand::palette(cx).text_muted)
                    .child("Steps come from T3's manager run and the executor thread that carries this request. Do not request another executor while it is in progress."),
            );
        }
        if let Some(note) = &progress.note {
            status = status.child(
                div()
                    .debug_selector(|| "project-launch-status-note".into())
                    .text_color(cx.theme().warning)
                    .child(note.clone()),
            );
        }
        if let Some(error) = &self.launch.status_error {
            status = status.child(div().text_color(cx.theme().danger).child(format!(
                "Cannot read launch status; the last confirmed step is kept. {error}"
            )));
        }
        status.into_any_element()
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
        // The native Manager has Rotation policy before Details.
        #[cfg(native_manager)]
        visual.simulate_keystrokes("down down down down down enter");
        #[cfg(not(native_manager))]
        visual.simulate_keystrokes("down down down down enter");
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

    const LAUNCH_FIXTURE: &str =
        include_str!("../../../okilum-t3/tests/fixtures/t3-launch-progress-2026-11-17.json");

    /// One recorded T3 frame, with `request_id` in the executor's first line.
    fn launch_frame(gateway: &fixture::Fake, request_id: &str, index: usize) {
        let recorded: serde_json::Value =
            serde_json::from_str(&LAUNCH_FIXTURE.replace("{{REQUEST_ID}}", request_id)).unwrap();
        *gateway.launch_t3.lock().unwrap() = Some(serde_json::json!({
            "frame": recorded["frames"][index],
            "timelines": recorded["timelines"],
            "receipt": recorded["receipt"],
        }));
    }

    fn status_line(view: &Entity<ProjectView>, visual: &mut VisualTestContext) -> String {
        view.read_with(visual, |v, _| {
            v.launch.tracker.as_ref().unwrap().progress.line()
        })
    }

    /// Lets the launch loop take its next T3 observation.
    fn next_poll(visual: &mut VisualTestContext) {
        visual
            .executor()
            .advance_clock(std::time::Duration::from_secs(16));
        visual.run_until_parked();
    }

    #[gpui::test]
    fn issue1420_launch_status_follows_recorded_t3_status_and_ignores_an_older_launch(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::test_init);
        let files = tempfile::tempdir().unwrap();
        let area = files.path().join("area");
        std::fs::create_dir_all(area.join("ops")).unwrap();
        std::fs::write(area.join("brief.md"), "# UI brief\nFixture request\n").unwrap();
        let gateway = Arc::new(fixture::Fake::default());
        launch_frame(&gateway, "not-yet-requested", 0);
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
                    (&view.launch.brief, "brief.md"),
                    (&view.launch.worktree, "/server/worktrees/ui"),
                    (&view.launch.branch, "feature/ui"),
                ] {
                    input.update(cx, |input, cx| input.set_value(value, window, cx));
                }
                view
            });
            entity = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = entity.unwrap();
        visual.run_until_parked();
        visual.update(|window, cx| view.update(cx, |v, cx| v.open_launch(window, cx)));
        visual.run_until_parked();
        visual.update(|window, cx| view.update(cx, |v, cx| v.submit_launch(window, cx)));
        visual.run_until_parked();
        let first = view.read_with(visual, |v, _| {
            assert!(v.records[0].receipt.is_some());
            Request::from_message(&v.records[0].message.text)
                .unwrap()
                .unwrap()
                .request_id
        });
        assert_eq!(
            status_line(&view, visual),
            "[requested] → accepted → preparing → running → completed"
        );
        assert!(
            visual.debug_bounds("project-launch-status-line").is_some(),
            "the status line replaces the dead-end requested text"
        );
        assert!(visual.debug_bounds("project-launch-requested").is_none());

        let steps = [
            "[requested] → accepted → preparing → running → completed",
            "requested → [accepted] → preparing → running → completed",
            "requested → accepted → [preparing] → running → completed",
            "requested → accepted → preparing → [running] → completed",
        ];
        for (index, expected) in steps.iter().enumerate() {
            launch_frame(&gateway, &first, index);
            next_poll(visual);
            assert_eq!(&status_line(&view, visual), expected, "frame {index}");
        }
        view.read_with(visual, |v, _| {
            let executor = v.launch.tracker.as_ref().unwrap().progress.executor.clone();
            assert_eq!(executor.unwrap().thread_id, "mcp:fixture-executor-new");
            assert_eq!(v.launch.polling.as_deref(), Some(first.as_str()));
        });
        let reads = gateway.launch_reads.lock().unwrap().clone();
        assert!(
            !reads.iter().any(|id| id == "mcp:fixture-executor-old"),
            "a thread from before the request is never inspected: {reads:?}"
        );

        // A separate request replaces the first. The recorded frame now shows
        // the first launch completed; that must not reach the second request.
        visual.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.launch.requested = None;
                v.launch.tracker = None;
                v.submit_launch(window, cx);
            })
        });
        visual.run_until_parked();
        let second = view.read_with(visual, |v, _| {
            let tracker = v.launch.tracker.as_ref().unwrap();
            assert_ne!(tracker.request_id, first);
            tracker.request_id.clone()
        });
        launch_frame(&gateway, &first, 4);
        for _ in 0..3 {
            next_poll(visual);
        }
        view.read_with(visual, |v, _| {
            let tracker = v.launch.tracker.as_ref().unwrap();
            assert_eq!(tracker.request_id, second);
            assert_eq!(
                tracker.progress.line(),
                "[requested] → accepted → preparing → running → completed",
                "the older launch's completed thread never reaches the newer request"
            );
            assert!(tracker.progress.executor.is_none());
            assert_eq!(v.launch.polling.as_deref(), Some(second.as_str()));
        });
        assert_eq!(gateway.sends.lock().unwrap().len(), 2);

        // The second request's own thread reports a final outcome; polling ends.
        let mut recorded: serde_json::Value =
            serde_json::from_str(&LAUNCH_FIXTURE.replace("{{REQUEST_ID}}", &second)).unwrap();
        let mut frame = recorded["frames"][4].clone();
        let mut thread = recorded["outcomes"]["failed"].clone();
        thread["threadId"] = "mcp:fixture-executor-second".into();
        thread["updatedAt"] = "2026-11-17T11:00:00.000Z".into();
        frame["executors"].as_array_mut().unwrap().push(thread);
        recorded["timelines"]["mcp:fixture-executor-second"] =
            recorded["timelines"]["mcp:fixture-executor-new"].clone();
        *gateway.launch_t3.lock().unwrap() = Some(serde_json::json!({
            "frame": frame, "timelines": recorded["timelines"], "receipt": recorded["receipt"],
        }));
        next_poll(visual);
        assert_eq!(
            status_line(&view, visual),
            "requested → accepted → [failed]",
            "only steps T3 confirmed for the second launch precede its outcome"
        );
        view.read_with(visual, |v, _| {
            let executor = v.launch.tracker.as_ref().unwrap().progress.executor.clone();
            assert_eq!(executor.unwrap().thread_id, "mcp:fixture-executor-second");
        });
        assert!(visual.debug_bounds("project-launch-status-line").is_some());
        next_poll(visual);
        view.read_with(visual, |v, _| {
            assert!(v.launch.polling.is_none(), "a final outcome ends polling")
        });
        assert_eq!(gateway.sends.lock().unwrap().len(), 2, "status never sends");
    }
}
