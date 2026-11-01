use super::*;
use gpui_component::popover::Popover;
use okilum_t3::{models::Target, ModelSelection};

#[cfg(target_os = "linux")]
pub(super) mod catalog;
#[cfg(test)]
mod tests;

#[derive(Default)]
pub(super) struct PickerUi {
    search: Option<Entity<InputState>>,
    changes: Option<Subscription>,
    provider: Option<String>,
    scroll: ScrollHandle,
    follow_current: bool,
}

impl ProjectView {
    #[cfg(target_os = "linux")]
    pub(super) fn receive_live_model(&mut self, metadata: &okilum_t3::projection::Metadata) {
        if self
            .chat
            .as_ref()
            .is_none_or(|chat| chat.thread_id != metadata.thread_id)
        {
            return;
        }
        let old = self.native.current_selection(&metadata.thread_id);
        if old == metadata.model_selection.as_ref()
            && self.native.current_model(&metadata.thread_id) == Some(metadata.model.as_str())
        {
            return;
        }
        self.native.model_revision = self.native.model_revision.wrapping_add(1);
        let Some(snapshot) = self
            .models
            .as_mut()
            .filter(|snapshot| snapshot.target.thread_id == metadata.thread_id)
        else {
            return;
        };
        let Some(selection) = &metadata.model_selection else {
            self.models = None;
            self.model_ui.provider = None;
            return;
        };
        if self.model_ui.provider.as_deref()
            == Some(&snapshot.configuration.model_selection.instance_id)
        {
            self.model_ui.provider = Some(selection.instance_id.clone());
        }
        snapshot.configuration.model_selection = selection.clone();
    }

    #[cfg(target_os = "linux")]
    fn reconcile_model_read(
        &self,
        snapshot: &mut okilum_t3::models::Snapshot,
        requested_revision: u64,
    ) {
        // A late catalog/readback cannot overwrite newer subscription metadata,
        // including A→B→A changes and option-only changes of the same model.
        if self.native.model_revision != requested_revision {
            if let Some(selection) = self.native.current_selection(&snapshot.target.thread_id) {
                snapshot.configuration.model_selection = selection.clone();
            }
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn ensure_native_catalog(&mut self, cx: &mut Context<Self>) {
        if self.native.catalog.is_some() {
            return;
        }
        let Some(client) = self.native.model_client() else {
            return;
        };
        let catalog = catalog::Catalog::acquire(client, cx);
        self.native.catalog_observer = Some(cx.observe(&catalog, |this, _, cx| {
            this.sync_native_models(cx);
            cx.notify();
        }));
        self.native.catalog = Some(catalog);
    }

    #[cfg(target_os = "linux")]
    pub(super) fn sync_native_models(&mut self, cx: &App) {
        let Some(target) = self.manager_model_target() else {
            return;
        };
        let Some(configuration) = self.native.configuration(&target.thread_id) else {
            return;
        };
        let Some(catalog) = &self.native.catalog else {
            return;
        };
        let catalog = catalog.read(cx);
        if !self.model_busy {
            self.model_error = catalog.error.clone();
        }
        if let Some(data) = &catalog.data {
            self.models = Some(okilum_t3::models::Snapshot {
                target: target.clone(),
                configuration: configuration.clone(),
                catalog: okilum_t3::Capabilities {
                    runtime_mode: configuration.runtime_mode,
                    providers: data.providers.clone(),
                },
            });
            self.model_target = Some(target);
        }
    }

    fn can_read_models(&self) -> bool {
        #[cfg(target_os = "linux")]
        if let Some(client) = self.native.model_client() {
            return client.access_allowed();
        }
        self.gateway.as_ref().is_some_and(|gateway| {
            matches!(
                gateway.status(),
                GatewayStatus::Connected { .. } | GatewayStatus::ExpiresSoon { .. }
            )
        })
    }

    fn can_write_models(&self) -> bool {
        #[cfg(target_os = "linux")]
        if let Some(client) = self.native.model_client() {
            return client.access_allowed() && client.can_operate();
        }
        self.identity.is_some() && self.can_read_models()
    }

    fn settle_model_layout(&self, focus_search: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.model_target.clone() else {
            return;
        };
        let generation = self.generation;
        // Platform callbacks run BEFORE draw. The second callback is after
        // the popup's first draw, when input and scroll geometry both exist.
        cx.on_next_frame(window, move |_, window, cx| {
            cx.on_next_frame(window, move |this, window, cx| {
                if this.generation != generation
                    || !this.model_open
                    || this.model_target.as_ref() != Some(&target)
                {
                    return;
                }
                let Some(input) = &this.model_ui.search else {
                    return;
                };
                if focus_search {
                    input.update(cx, |input, cx| input.focus(window, cx));
                }
                if input.read(cx).value().trim().is_empty() {
                    if let Some(index) = this.models.as_ref().and_then(|snapshot| {
                        snapshot
                            .picker_choices(this.model_ui.provider.as_deref().unwrap_or(""), "")
                            .iter()
                            .position(|choice| choice.current)
                    }) {
                        this.model_ui.scroll.scroll_to_item(index);
                    }
                }
                cx.notify();
            });
        });
    }

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
        if self.busy || !self.can_read_models() {
            return;
        }
        if self.model_target.as_ref() != Some(&target) {
            self.models = None;
            self.model_busy = false;
            self.model_error = None;
        }
        self.model_target = Some(target);
        self.model_ui.follow_current = true;
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
        self.settle_model_layout(true, window, cx);
        self.model_candidate = None;
        #[cfg(target_os = "linux")]
        self.sync_native_models(cx);
        cx.notify();
    }

    pub(super) fn model_request(
        &mut self,
        selection: Option<ModelSelection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.model_request_inner(selection, false, window, cx);
    }

    pub(super) fn warm_gateway_models(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The same completed poll just verified thread/project ownership.
        // Avoid a second history read solely to warm the model catalog.
        self.model_request_inner(None, true, window, cx);
    }

    fn model_request_inner(
        &mut self,
        selection: Option<ModelSelection>,
        verified_poll: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        #[cfg(target_os = "linux")]
        if selection.is_none() && self.native.active() {
            if let Some(catalog) = &self.native.catalog {
                catalog.update(cx, |catalog, cx| catalog.refresh(cx));
            }
            self.sync_native_models(cx);
            cx.notify();
            return;
        }
        if self.model_busy
            || self.busy
            || (if selection.is_some() {
                !self.can_write_models()
            } else {
                !self.can_read_models()
            })
        {
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
        #[cfg(target_os = "linux")]
        let requested_revision = self.native.model_revision;
        #[cfg(target_os = "linux")]
        let requested_connection = self.native.connection_epoch;
        #[cfg(target_os = "linux")]
        let requested_scope = self
            .native
            .model_client()
            .map(|client| client.catalog_scope());
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
                        None if verified_poll => (|| {
                            let catalog = gateway.capabilities()?;
                            let configuration = gateway.thread_configuration(&target.thread_id)?;
                            anyhow::ensure!(configuration.thread_id == target.thread_id, "T3 returned another thread's model configuration");
                            Ok(okilum_t3::models::Snapshot { target: target.clone(), catalog, configuration })
                        })(),
                        None => target.load(gateway.as_ref()),
                    };
                    let observed = if applying && result.is_err() { target.load(gateway.as_ref()).ok() } else { None };
                    (result, observed)
                })
                .await;
            let (result, observed) = result;
            let _ = this.update_in(cx, |this, window, cx| {
                #[cfg(target_os = "linux")]
                if requested_connection != this.native.connection_epoch || requested_scope != this.native.model_client().map(|client| client.catalog_scope()) { return; }
                if this.generation == generation
                    && this.model_target.as_ref() == Some(&requested_target)
                {
                    this.model_busy = false;
                    match result {
                        Ok(snapshot) => {
                            #[cfg(target_os = "linux")]
                            let snapshot = {
                                let mut snapshot = snapshot;
                                this.reconcile_model_read(&mut snapshot, requested_revision);
                                snapshot
                            };
                            if this.model_ui.follow_current || this.model_ui.provider.is_none() {
                                this.model_ui.provider = Some(snapshot.configuration.model_selection.instance_id.clone());
                            }
                            this.model_ui.follow_current = false;
                            this.models = Some(snapshot);
                            this.settle_model_layout(false, window, cx);
                            this.model_candidate = None;
                            this.model_error = None;
                        }
                        Err(error) => {
                            if let Some(observed) = observed {
                                #[cfg(target_os = "linux")]
                                let observed = {
                                    let mut observed = observed;
                                    this.reconcile_model_read(&mut observed, requested_revision);
                                    observed
                                };
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
                            eprintln!("Manager model request failed ({action}): {error:#}");
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
        let search_focus = self
            .model_ui
            .search
            .as_ref()
            .map(|input| input.read(cx).focus_handle(cx));
        Popover::new(id)
            .when_some(search_focus.as_ref(), |popover, focus| {
                popover.track_focus(focus)
            })
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
                    .disabled(self.busy || !self.can_read_models()),
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
        if self.model_busy || self.busy || !self.can_write_models() {
            return;
        }
        self.model_candidate = Some(selection.clone());
        self.model_open = false;
        self.compose.update(cx, |input, cx| input.focus(window, cx));
        self.model_request(Some(selection), window, cx);
    }
}

mod picker;
