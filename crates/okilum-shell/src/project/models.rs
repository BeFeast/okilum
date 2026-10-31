use super::*;
use gpui_component::popover::Popover;
use okilum_t3::{models::Target, ModelSelection};

#[cfg(test)]
mod tests;

#[derive(Default)]
pub(super) struct PickerUi {
    search: Option<Entity<InputState>>,
    changes: Option<Subscription>,
    provider: Option<String>,
}

impl ProjectView {
    pub(super) fn manager_model_target(&self) -> Option<Target> {
        let binding = self.binding.as_ref()?;
        #[cfg(target_os = "linux")]
        if self.native.active() {
            return Some(Target {
                thread_id: self.chat.as_ref()?.thread_id.clone(),
                project_id: binding.manager.project_id.clone()?,
            });
        }
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
        self.model_ui.provider = self
            .models
            .as_ref()
            .map(|snapshot| snapshot.configuration.model_selection.instance_id.clone());
        if self.model_ui.search.is_none() {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search models…"));
            self.model_ui.changes =
                Some(cx.subscribe(&input, |_, _, _: &InputEvent, cx| cx.notify()));
            self.model_ui.search = Some(input);
        }
        self.model_ui
            .search
            .as_ref()
            .unwrap()
            .update(cx, |input, cx| {
                input.set_value("", window, cx);
                input.focus(window, cx);
            });
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
        let gateway = self.gateway.clone();
        #[cfg(target_os = "linux")]
        let native = self.native.model_client();
        let generation = self.generation;
        let applying = selection.is_some();
        self.model_busy = true;
        self.model_error = None;
        cx.spawn_in(window, async move |this, cx| {
            let requested_target = target.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    #[cfg(target_os = "linux")]
                    if let Some(client) = native {
                        let result = match selection {
                            Some(selection) => client.configure_manager_model(&target,&selection),
                            None => client.manager_models(&target),
                        }.map_err(anyhow::Error::from);
                        let observed = if applying && result.is_err() { client.manager_models(&target).ok() } else { None };
                        return (result, observed);
                    }
                    let Some(gateway) = gateway else { return (Err(anyhow::anyhow!("Not connected")), None); };
                    let result = match selection {
                        Some(selection) => target.configure(gateway.as_ref(), &selection),
                        None => target.load(gateway.as_ref()),
                    };
                    let observed = if applying && result.is_err() { target.load(gateway.as_ref()).ok() } else { None };
                    (result, observed)
                })
                .await;
            let (result, observed) = result;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.generation == generation
                    && this.model_target.as_ref() == Some(&requested_target)
                {
                    this.model_busy = false;
                    match result {
                        Ok(snapshot) => {
                            if this.model_ui.provider.is_none() {
                                this.model_ui.provider = Some(snapshot.configuration.model_selection.instance_id.clone());
                            }
                            this.models = Some(snapshot);
                            this.model_candidate = None;
                            this.model_error = None;
                        }
                        Err(error) => {
                            if let Some(observed) = observed {
                                this.model_ui.provider = Some(observed.configuration.model_selection.instance_id.clone());
                                this.models = Some(observed);
                            }
                            if applying {
                                this.model_open = true;
                                if let Some(input) = &this.model_ui.search {
                                    input.update(cx, |input, cx| input.focus(window, cx));
                                }
                            }
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
            .unwrap_or_else(|| {
                #[cfg(target_os = "linux")]
                if self
                    .chat
                    .as_ref()
                    .is_some_and(|chat| chat.thread_id == target.thread_id)
                {
                    if let Some(model) = self.native.current_model(&target.thread_id) {
                        return model.into();
                    }
                }
                "Model".into()
            });
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
                        if !this.model_open {
                            this.open_models(target.clone(), window, cx);
                        }
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

    fn choose_model(
        &mut self,
        selection: ModelSelection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.model_busy || !self.can_send() {
            return;
        }
        self.model_candidate = Some(selection.clone());
        self.model_open = false;
        self.compose.update(cx, |input, cx| input.focus(window, cx));
        self.model_request(Some(selection), window, cx);
    }
}

mod picker;
