use super::*;
use gpui_component::popover::Popover;
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
        if self.model_target.as_ref() != Some(&target) {
            self.models = None;
        }
        self.model_target = Some(target);
        self.model_open = true;
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
        let applying = selection.is_some();
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
                        Err(error) => {
                            let action = if applying {
                                "Model change not confirmed"
                            } else {
                                "Cannot read thread models; no change was requested"
                            };
                            this.model_error = Some(format!(
                                "{action}. {error:#} Refresh to read the current configuration; no automatic retry."
                            ));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn model_picker(
        &self,
        id: impl Into<ElementId>,
        target: Target,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let is_target = self.model_target.as_ref() == Some(&target);
        let label = self
            .models
            .as_ref()
            .filter(|_| is_target)
            .map(|snapshot| {
                let current = &snapshot.configuration.model_selection;
                snapshot
                    .catalog
                    .providers
                    .iter()
                    .find(|provider| provider.provider_instance_id == current.instance_id)
                    .and_then(|provider| {
                        provider
                            .models
                            .iter()
                            .find(|model| model.id == current.model)
                    })
                    .and_then(|model| model.label.clone())
                    .unwrap_or_else(|| current.model.clone())
            })
            .unwrap_or_else(|| "Model".into());
        let owner = cx.entity().downgrade();
        Popover::new(id)
            .anchor(Anchor::TopRight)
            .open(self.model_open && is_target)
            .trigger(
                Button::new("project-model-trigger")
                    .debug_selector(|| "project-manager-model".into())
                    .ghost()
                    .small()
                    .max_w(px(180.))
                    .overflow_hidden()
                    .label(label)
                    .icon(IconName::ChevronDown)
                    .tooltip("Choose model")
                    .disabled(self.busy || !self.can_send()),
            )
            .on_open_change(move |open, window, cx| {
                let _ = owner.update(cx, |this, cx| {
                    if *open {
                        this.open_models(target.clone(), window, cx);
                    } else {
                        this.model_open = false;
                        cx.notify();
                    }
                });
            })
            .child(if self.model_open && is_target {
                self.render_models(cx)
            } else {
                div().into_any_element()
            })
            .into_any_element()
    }

    pub(super) fn render_models(&self, cx: &mut Context<Self>) -> AnyElement {
        let enabled = !self.model_busy && !self.busy && self.can_send();
        let mut panel = v_flex()
            .debug_selector(|| "project-model-panel".into())
            .w(px(272.))
            .max_h(px(320.))
            .gap_2()
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .child("Model")
                    .child(
                        Button::new("project-model-refresh")
                            .debug_selector(|| "project-model-refresh".into())
                            .ghost()
                            .small()
                            .icon(IconName::RotateCw)
                            .tooltip("Refresh models")
                            .disabled(!enabled)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.model_request(None, window, cx)
                            })),
                    ),
            );
        if self.model_busy {
            panel = panel.child("Loading…");
        }
        if let Some(error) = &self.model_error {
            panel = panel.child(
                div()
                    .debug_selector(|| "project-model-error".into())
                    .text_color(cx.theme().danger)
                    .child(if self.connection_details {
                        error.clone()
                    } else {
                        "Could not confirm the model. Refresh or open T3 to continue.".into()
                    }),
            );
        }
        if let Some(snapshot) = &self.models {
            let current = &snapshot.configuration.model_selection;
            let mut choices = v_flex()
                .id("project-model-choices")
                .min_h_0()
                .max_h(px(200.))
                .overflow_y_scroll()
                .gap_1();
            let mut index = 0usize;
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
                            .w_full()
                            .justify_start()
                            .overflow_hidden()
                            .icon(if selected {
                                IconName::Check
                            } else {
                                IconName::Bot
                            })
                            .label(model.label.as_deref().unwrap_or(&model.id).to_owned())
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
                choices = choices.child("No models available.");
            }
            panel = panel.child(choices).child(
                Button::new("project-model-apply")
                    .debug_selector(|| "project-model-apply".into())
                    .w_full()
                    .label("Apply")
                    .disabled(!enabled || self.model_candidate.is_none())
                    .on_click(cx.listener(|this, _, window, cx| {
                        if let Some(selection) = this.model_candidate.clone() {
                            this.model_request(Some(selection), window, cx);
                        }
                    })),
            );
        }
        if self.connection_details {
            panel = panel.child(
                div()
                    .debug_selector(|| "project-model-scheduler-limit".into())
                    .child(okilum_t3::models::SCHEDULED_TASK_LIMIT),
            );
        }
        panel.into_any_element()
    }
}
