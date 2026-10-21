use super::*;
use okilum_t3::{models::Target, ModelSelection};

impl ProjectView {
    pub(super) fn manager_model_target(&self) -> Option<Target> {
        let binding = self.binding.as_ref()?;
        Some(Target {
            thread_id: binding.manager.thread_id.clone()?,
            project_id: binding.manager.project_id.clone()?,
        })
    }

    pub(super) fn open_models(
        &mut self,
        target: Target,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.model_busy || self.busy {
            return;
        }
        self.model_target = Some(target);
        self.models = None;
        self.model_candidate = None;
        self.model_request(None, window, cx);
    }

    fn model_request(
        &mut self,
        selection: Option<ModelSelection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.model_busy || self.busy || !self.can_send() {
            return;
        }
        let Some(target) = self.model_target.clone() else {
            return;
        };
        let Some(gateway) = self.gateway.clone() else {
            return;
        };
        let generation = self.generation;
        self.model_busy = true;
        self.model_error = None;
        cx.spawn_in(window, async move |this, cx| {
            let requested_target = target.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    match selection {
                        Some(selection) => target.configure(gateway.as_ref(), &selection),
                        None => target.load(gateway.as_ref()),
                    }
                })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                this.model_busy = false;
                if this.generation == generation
                    && this.model_target.as_ref() == Some(&requested_target)
                {
                    match result {
                        Ok(snapshot) => {
                            this.models = Some(snapshot);
                            this.model_candidate = None;
                            this.model_error = None;
                        }
                        Err(error) => this.model_error = Some(format!("{error:#}")),
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn render_models(&self, cx: &mut Context<Self>) -> AnyElement {
        let target = self.model_target.as_ref().expect("open model panel");
        let enabled = !self.model_busy && !self.busy && self.can_send();
        let mut panel = v_flex()
            .debug_selector(|| "project-model-panel".into())
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(format!("Thread model · {}", target.thread_id))
                    .child(
                        Button::new("project-model-refresh")
                            .ghost()
                            .label("Refresh models")
                            .disabled(!enabled)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.model_request(None, window, cx)
                            })),
                    )
                    .child(
                        Button::new("project-model-close")
                            .ghost()
                            .label("Close")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.model_target = None;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .debug_selector(|| "project-model-scheduler-limit".into())
                    .child(okilum_t3::models::SCHEDULED_TASK_LIMIT),
            );
        if self.model_busy {
            panel = panel.child("Waiting for T3 configuration readback…");
        }
        if let Some(error) = &self.model_error {
            panel = panel.child(div().debug_selector(|| "project-model-error".into())
                .text_color(cx.theme().danger)
                .child(format!("Model change not confirmed. {error} Refresh to read the current model; no automatic retry.")));
        }
        if let Some(snapshot) = &self.models {
            let current = &snapshot.configuration.model_selection;
            panel = panel.child(
                div()
                    .debug_selector(|| "project-model-confirmed".into())
                    .child(format!(
                        "Last confirmed readback: {} / {} · thread {} · credential {}",
                        current.instance_id,
                        current.model,
                        snapshot.configuration.runtime_mode,
                        snapshot.catalog.runtime_mode
                    )),
            );
            let mut choices = v_flex()
                .id("project-model-choices")
                .max_h(px(140.))
                .overflow_y_scroll()
                .gap_1();
            let mut index = 0;
            for provider in &snapshot.catalog.providers {
                for model in &provider.models {
                    let candidate = snapshot.selection(&provider.provider_instance_id, &model.id);
                    let valid = snapshot.validate(&candidate).is_ok();
                    let selected = self.model_candidate.as_ref().unwrap_or(current) == &candidate;
                    let id = index;
                    index += 1;
                    choices = choices.child(
                        Button::new(("project-model-choice", id))
                            .debug_selector(move || format!("project-model-choice-{id}"))
                            .ghost()
                            .label(format!(
                                "{} / {}",
                                provider
                                    .display_name
                                    .as_deref()
                                    .unwrap_or(&provider.provider_instance_id),
                                model.label.as_deref().unwrap_or(&model.id)
                            ))
                            .selected(selected)
                            .disabled(!enabled || !valid)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.model_candidate = Some(candidate.clone());
                                cx.notify();
                            })),
                    );
                }
            }
            if index == 0 {
                choices = choices.child("No models returned by the live T3 catalog.");
            }
            panel = panel.child(choices)
                .child("Selecting a different model clears previous model option overrides and uses T3 defaults.")
                .child(Button::new("project-model-apply").debug_selector(|| "project-model-apply".into()).ghost()
                    .label("Apply thread model")
                    .disabled(!enabled || self.model_candidate.is_none())
                    .on_click(cx.listener(|this, _, window, cx| {
                        if let Some(selection) = this.model_candidate.clone() {
                            this.model_request(Some(selection), window, cx);
                        }
                    })));
        }
        panel.into_any_element()
    }
}
