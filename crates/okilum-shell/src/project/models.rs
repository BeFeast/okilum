use super::*;
use gpui_component::ThemeStyled as _;
use okilum_t3::{models::Target, ModelSelection};

#[cfg(native_manager)]
pub(super) mod catalog;
#[cfg(test)]
mod interaction_tests;
#[cfg(test)]
mod layout_tests;
mod navigation;
mod options;
mod preferences;
#[cfg(test)]
mod tests;
mod visuals;

#[cfg(any(test, feature = "project-ui-harness"))]
type SurfacePaint = (Bounds<Pixels>, Bounds<Pixels>);

#[derive(Default)]
pub(super) struct PickerUi {
    search: Option<Entity<InputState>>,
    changes: Option<Subscription>,
    provider: Option<String>,
    follow_current: bool,
    favorites: bool,
    highlight: usize,
    list_scroll: gpui::UniformListScrollHandle,
    option_scroll: gpui::ScrollHandle,
    panel_width: std::rc::Rc<std::cell::Cell<Pixels>>,
    panel_height: std::cell::Cell<Pixels>,
    preferences: Option<preferences::Prefs>,
    preferences_origin: Option<String>,
    overlay: Option<gpui_base::DeferredPopover>,
    #[cfg(any(test, feature = "project-ui-harness"))]
    painted_surface: std::rc::Rc<std::cell::Cell<Option<SurfacePaint>>>,
    #[cfg(test)]
    diagnostics_painted: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
}

/// Strip a redundant provider prefix only when it is confirmed by real
/// metadata. A model name containing an unrelated middle dot is left intact.
fn compact_chip_name<'a>(name: &'a str, provider: Option<&str>, upstream: Option<&str>) -> &'a str {
    let Some((prefix, model)) = name.split_once('·') else {
        return name;
    };
    let model = model.trim();
    if !model.is_empty()
        && [provider, upstream]
            .into_iter()
            .flatten()
            .any(|actual| prefix.trim().eq_ignore_ascii_case(actual.trim()))
    {
        model
    } else {
        name
    }
}

impl ProjectView {
    fn close_models(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(feature = "project-ui-harness")]
        if std::env::var_os("OKILUM_PROJECT_UI_FIXTURE").is_some() {
            diagnostic_error!("QA model-picker close");
        }
        self.model_open = false;
        self.model_ui.overlay.take();
        self.compose.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn load_model_preferences(&mut self) {
        let Some(environment) = &self.environment else {
            return;
        };
        let origin = environment.origin();
        if self.model_ui.preferences_origin.as_deref() == Some(&origin)
            && self
                .model_ui
                .preferences
                .as_ref()
                .is_none_or(|prefs| prefs.origin() == origin)
        {
            return;
        }
        self.model_ui.preferences_origin = Some(origin.clone());
        self.model_ui.preferences = None;
        let Some(state) = self
            .state
            .clone()
            .or_else(|| reader_history::state_directory().ok())
        else {
            return;
        };
        match preferences::Prefs::load(&state, &origin) {
            Ok(prefs) => self.model_ui.preferences = Some(prefs),
            Err(_) => self.model_error = Some("Saved model preferences could not be read.".into()),
        }
    }

    fn model_is_favorite(&self, selection: &ModelSelection) -> bool {
        self.model_ui
            .preferences
            .as_ref()
            .is_some_and(|prefs| prefs.is_favorite(&selection.instance_id, &selection.model))
    }

    fn toggle_model_favorite(&mut self, selection: ModelSelection, cx: &mut Context<Self>) {
        let Some(prefs) = &mut self.model_ui.preferences else {
            return;
        };
        prefs.toggle_favorite(&selection.instance_id, &selection.model);
        if prefs.save().is_err() {
            self.model_error = Some("Could not save model favorites.".into());
        }
        cx.notify();
    }

    fn remember_model_selection(&mut self, selection: &ModelSelection) {
        if let Some(prefs) = &mut self.model_ui.preferences {
            if prefs
                .remember(selection)
                .and_then(|_| prefs.save())
                .is_err()
            {
                self.model_error = Some("Could not save model preferences.".into());
            }
        }
    }

    fn visible_model_choices(&self, cx: &App) -> Vec<okilum_t3::models::PickerChoice> {
        let Some(snapshot) = &self.models else {
            return Vec::new();
        };
        let query = self
            .model_ui
            .search
            .as_ref()
            .map(|input| input.read(cx).value().to_string())
            .unwrap_or_default();
        let instance = self
            .model_ui
            .provider
            .as_deref()
            .unwrap_or(&snapshot.configuration.model_selection.instance_id);
        let mut choices = if self.model_ui.favorites && query.trim().is_empty() {
            let mut rows = snapshot
                .catalog
                .providers
                .iter()
                .flat_map(|provider| snapshot.picker_choices(&provider.provider_instance_id, ""))
                .filter(|choice| choice.current || self.model_is_favorite(&choice.selection))
                .collect::<Vec<_>>();
            if !rows.iter().any(|choice| choice.current) {
                rows.extend(
                    snapshot
                        .picker_choices(&snapshot.configuration.model_selection.instance_id, "")
                        .into_iter()
                        .filter(|choice| choice.current),
                );
            }
            rows.sort_by_key(|choice| !choice.current);
            rows
        } else {
            snapshot.picker_choices(instance, &query)
        };
        if !query.trim().is_empty() {
            choices.sort_by_key(|choice| !self.model_is_favorite(&choice.selection));
        }
        choices
    }

    pub(super) fn model_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        // Dismissal is local UI state. A send, lost connection or unavailable
        // catalog must never trap an already-open popup or dispatch a request.
        if self.model_open
            && event.keystroke.key == "escape"
            && event.keystroke.modifiers == gpui::Modifiers::default()
        {
            self.close_models(window, cx);
            cx.stop_propagation();
            return true;
        }
        if !self.active || self.busy || !self.can_read_models() {
            return false;
        }
        if self.compose.read(cx).ime_composing()
            || self
                .model_ui
                .search
                .as_ref()
                .is_some_and(|input| input.read(cx).ime_composing())
        {
            return false;
        }
        // Enter on a focused Star/provider/option button belongs to that
        // control. Search owns list navigation; numbered shortcuts and Escape
        // still work throughout the open picker.
        if self.model_open
            && matches!(event.keystroke.key.as_str(), "enter" | "up" | "down")
            && self
                .model_ui
                .search
                .as_ref()
                .is_none_or(|input| !input.read(cx).focus_handle(cx).is_focused(window))
        {
            return false;
        }
        let choices = self.visible_model_choices(cx);
        let Some(intent) =
            navigation::handle(event, self.model_open, self.model_ui.highlight, &choices)
        else {
            return false;
        };
        match intent {
            navigation::Intent::Toggle if self.model_open => {
                self.close_models(window, cx);
            }
            navigation::Intent::Toggle => {
                if let Some(target) = self.manager_model_target() {
                    self.open_models(target, window, cx);
                }
            }
            navigation::Intent::Close => {
                self.close_models(window, cx);
            }
            navigation::Intent::Highlight(index) => {
                self.model_ui.highlight = index;
                self.model_ui
                    .list_scroll
                    .scroll_to_item(index, gpui::ScrollStrategy::Center);
            }
            navigation::Intent::Select(index) => {
                if let Some(choice) = choices.get(index) {
                    self.choose_model(choice.selection.clone(), window, cx);
                }
            }
        }
        cx.stop_propagation();
        cx.notify();
        true
    }

    #[cfg(native_manager)]
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

    #[cfg(native_manager)]
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

    #[cfg(native_manager)]
    pub(super) fn ensure_native_catalog(&mut self, cx: &mut Context<Self>) {
        if self.native.catalog.is_some() {
            return;
        }
        let Some(client) = self.native.model_client() else {
            return;
        };
        self.load_model_preferences();
        let catalog = catalog::Catalog::acquire(client, cx);
        self.native.catalog_observer = Some(cx.observe(&catalog, |this, _, cx| {
            this.sync_native_models(cx);
            cx.notify();
        }));
        self.native.catalog = Some(catalog);
    }

    #[cfg(native_manager)]
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
        #[cfg(native_manager)]
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
        #[cfg(native_manager)]
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
                    if let Some(index) = this
                        .visible_model_choices(cx)
                        .iter()
                        .position(|choice| choice.current)
                    {
                        this.model_ui.highlight = index;
                        this.model_ui
                            .list_scroll
                            .scroll_to_item(index, gpui::ScrollStrategy::Center);
                    }
                }
                cx.notify();
            });
        });
    }

    pub(super) fn manager_model_target(&self) -> Option<Target> {
        let binding = self.binding.as_ref()?;
        #[cfg(native_manager)]
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
            self.model_ui.changes = Some(cx.subscribe(&input, |this, _, _: &InputEvent, cx| {
                this.model_ui.highlight = 0;
                this.model_ui
                    .list_scroll
                    .0
                    .borrow()
                    .base_handle
                    .set_offset(point(px(0.), px(0.)));
                cx.notify();
            }));
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
        self.model_ui.favorites = self
            .model_ui
            .preferences
            .as_ref()
            .is_some_and(preferences::Prefs::has_favorites);
        self.model_ui.highlight = self
            .visible_model_choices(cx)
            .iter()
            .position(|choice| choice.current)
            .unwrap_or(0);
        #[cfg(feature = "project-ui-harness")]
        if std::env::var_os("OKILUM_PROJECT_UI_FIXTURE").is_some() {
            diagnostic_error!(
                "QA model-picker open: catalog_ready={}",
                self.models.is_some()
            );
        }
        self.model_open = true;
        self.model_ui
            .panel_width
            .set((window.viewport_size().width - px(48.)).max(px(0.)));
        self.model_ui
            .panel_height
            .set((window.viewport_size().height - px(48.)).max(px(0.)));
        self.model_ui
            .overlay
            .get_or_insert_with(|| gpui_base::GlobalState::register_deferred_popover(cx));
        self.settle_model_layout(true, window, cx);
        self.model_candidate = None;
        #[cfg(native_manager)]
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
        self.load_model_preferences();
        self.model_request_inner(None, true, window, cx);
    }

    fn model_request_inner(
        &mut self,
        selection: Option<ModelSelection>,
        verified_poll: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        #[cfg(native_manager)]
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
        #[cfg(native_manager)]
        let native = self.native.model_client();
        let generation = self.generation;
        let applying = selection.is_some();
        #[cfg(native_manager)]
        let requested_revision = self.native.model_revision;
        #[cfg(native_manager)]
        let requested_connection = self.native.connection_epoch;
        #[cfg(native_manager)]
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
                    #[cfg(native_manager)]
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
                #[cfg(native_manager)]
                if requested_connection != this.native.connection_epoch || requested_scope != this.native.model_client().map(|client| client.catalog_scope()) { return; }
                if this.generation == generation
                    && this.model_target.as_ref() == Some(&requested_target)
                {
                    this.model_busy = false;
                    match result {
                        Ok(snapshot) => {
                            #[cfg(native_manager)]
                            let snapshot = {
                                let mut snapshot = snapshot;
                                this.reconcile_model_read(&mut snapshot, requested_revision);
                                snapshot
                            };
                            if this.model_ui.follow_current || this.model_ui.provider.is_none() {
                                this.model_ui.provider = Some(snapshot.configuration.model_selection.instance_id.clone());
                            }
                            this.model_ui.follow_current = false;
                            this.remember_model_selection(&snapshot.configuration.model_selection);
                            this.models = Some(snapshot);
                            this.settle_model_layout(false, window, cx);
                            this.model_candidate = None;
                            this.model_error = None;
                        }
                        Err(error) => {
                            if let Some(observed) = observed {
                                #[cfg(native_manager)]
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
                                this.model_ui.overlay.get_or_insert_with(||gpui_base::GlobalState::register_deferred_popover(cx));
                                if let Some(input) = &this.model_ui.search {
                                    input.update(cx, |input, cx| input.focus(window, cx));
                                }
                            }
                            let action = if applying {
                                "Model change not confirmed"
                            } else {
                                "Cannot read thread models; no change was requested"
                            };
                            diagnostic_error!("Manager model request failed ({action}): {error:#}");
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
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.model_ui
            .panel_width
            .set((window.viewport_size().width - px(48.)).max(px(0.)));
        self.model_ui
            .panel_height
            .set((window.viewport_size().height - px(48.)).max(px(0.)));
        let is_target = self.model_target.as_ref() == Some(&target);
        let (label, full_label) = self
            .models
            .as_ref()
            .filter(|_| is_target)
            .map(|snapshot| {
                let current = &snapshot.configuration.model_selection;
                let model = snapshot.model_info(current);
                let name = model
                    .map(|model| model.display_name())
                    .unwrap_or(&current.model);
                let provider = snapshot
                    .catalog
                    .providers
                    .iter()
                    .find(|provider| provider.provider_instance_id == current.instance_id);
                let compact = compact_chip_name(
                    name,
                    provider.and_then(|provider| provider.display_name.as_deref()),
                    model.and_then(|model| model.sub_provider.as_deref()),
                )
                .to_owned();
                let full_name = model
                    .and_then(|model| model.label.as_deref())
                    .filter(|label| !label.trim().is_empty())
                    .unwrap_or(name);
                let summary = self.model_chip_summary(snapshot, cx);
                let full = if summary.is_empty() {
                    full_name.to_owned()
                } else {
                    format!("{full_name} · {summary}")
                };
                (compact, full)
            })
            .unwrap_or_else(|| {
                #[cfg(native_manager)]
                if let Some(model) = self.native.current_model(&target.thread_id) {
                    return (model.to_owned(), model.to_owned());
                }
                ("Model".into(), "Model".into())
            });
        let glyph = self
            .models
            .as_ref()
            .filter(|_| is_target)
            .and_then(|snapshot| {
                snapshot.catalog.providers.iter().find(|provider| {
                    provider.provider_instance_id
                        == snapshot.configuration.model_selection.instance_id
                })
            })
            .map(|provider| provider.driver_kind.clone());
        let trigger_target = target.clone();
        let trigger = Button::new("project-model-trigger")
            .debug_selector(|| "project-manager-model".into())
            .ghost()
            .small()
            .h(px(32.))
            .border_1()
            .border_color(brand::palette(cx).border_subtle)
            .bg(brand::palette(cx).surface)
            .rounded(px(crate::project::design::RADIUS))
            .max_w(px(280.))
            .overflow_hidden()
            .accessibility_label(format!("Choose model; current: {full_label}"))
            .child(
                h_flex()
                    .min_w_0()
                    .items_center()
                    .gap_1()
                    .when_some(glyph, |row, driver| {
                        row.child(visuals::provider_visual(&driver, px(18.)))
                    })
                    .child(div().min_w_0().truncate().child(label.clone()))
                    .child(Icon::new(IconName::ChevronDown).size(px(18.))),
            )
            .selected(self.model_open && is_target)
            .tooltip(format!("Choose model · {full_label}"))
            .disabled(self.busy || !self.can_read_models())
            // Open after the click completes. A mouse-down-owned popover can
            // appear underneath the in-flight pointer release after layout.
            .on_click(cx.listener(move |this, _, window, cx| {
                if this.model_open {
                    this.close_models(window, cx);
                } else {
                    this.open_models(trigger_target.clone(), window, cx);
                }
            }));
        // The surface uses the app's existing deferred/anchored path, with
        // window-relative geometry independent of chip and catalog layout.
        let popup = div().id(id).child(trigger);
        if self.model_open && is_target {
            let content = v_flex()
                .id("project-model-popup-surface")
                .role(gpui::Role::Dialog)
                .occlude()
                .on_prepaint(gpui_base::GlobalState::record_native_overlay)
                .tab_group()
                .key_context("ManagerModels")
                .on_action(
                    cx.listener(|this, _: &crate::DismissManagerModel, window, cx| {
                        if this.model_open {
                            this.close_models(window, cx);
                            cx.stop_propagation();
                        } else {
                            cx.propagate();
                        }
                    }),
                )
                // Reader's Escape binding dispatches Dismiss before low-level
                // key listeners. Own it inside this deferred popup so it cannot
                // dismiss background Reader UI or trap focus in model search.
                .on_action(cx.listener(|this, _: &crate::Dismiss, window, cx| {
                    if this.model_open {
                        this.close_models(window, cx);
                        cx.stop_propagation();
                    } else {
                        cx.propagate();
                    }
                }))
                .popover_style(cx)
                .rounded(px(crate::project::design::RADIUS))
                .border_1()
                .border_color(brand::palette(cx).border_subtle)
                .text_size(px(brand::READER_CHROME_FONT_SIZE))
                .line_height(px(20.))
                .p(px(crate::project::design::INSET))
                .when_some(self.model_ui.search.as_ref(), |content, input| {
                    content.track_focus(&input.read(cx).focus_handle(cx))
                })
                .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close_models(window, cx)))
                .child(self.render_models(cx));
            #[cfg(any(test, feature = "project-ui-harness"))]
            let content = content.child({
                let painted = self.model_ui.painted_surface.clone();
                gpui::canvas(
                    |bounds, _, _| bounds,
                    move |_, bounds, window, _| {
                        let mask = window.content_mask().bounds;
                        painted.set(Some((bounds, mask)));
                        #[cfg(feature = "project-ui-harness")]
                        if std::env::var_os("OKILUM_PROJECT_UI_FIXTURE").is_some() {
                            diagnostic_error!("QA model-picker paint: bounds={bounds:?} mask={mask:?} viewport={:?}", window.viewport_size());
                        }
                    },
                )
                .absolute()
                .size_full()
            });
            popup
                .child(
                    gpui::deferred(
                        gpui::anchored()
                            // A fixed window-relative origin cannot follow chip
                            // label widths or composer/catalog state changes.
                            .position({
                                let viewport = window.viewport_size();
                                let panel = picker::surface_size(size(
                                    viewport.width - px(48.),
                                    viewport.height - px(48.),
                                ));
                                let chrome = px(crate::project::design::INSET * 2. + 2.);
                                point(
                                    ((viewport.width - panel.width - chrome) * 0.5).max(px(12.)),
                                    ((viewport.height - panel.height - chrome) * 0.5).max(px(12.)),
                                )
                            })
                            .snap_to_window_with_margin(px(12.))
                            .child(content),
                    )
                    .with_priority(gpui_base::POPUP_PRIORITY),
                )
                .into_any_element()
        } else {
            popup.into_any_element()
        }
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
        if let Some(current) = self
            .models
            .as_ref()
            .map(|snapshot| snapshot.configuration.model_selection.clone())
        {
            self.remember_model_selection(&current);
        }
        let selection = if let Some(snapshot) = &self.models {
            let current = &snapshot.configuration.model_selection;
            self.model_ui
                .preferences
                .as_ref()
                .and_then(|prefs| prefs.remembered(&selection))
                .filter(|saved| {
                    (current.instance_id != saved.instance_id || current.model != saved.model)
                        && snapshot.validate(saved).is_ok()
                })
                .unwrap_or(selection)
        } else {
            selection
        };
        self.model_candidate = Some(selection.clone());
        self.close_models(window, cx);
        self.model_request(Some(selection), window, cx);
    }
}

#[cfg(test)]
mod compact_chip_tests {
    use super::compact_chip_name;
    use ::core::prelude::v1::test;

    #[test]
    fn only_confirmed_provider_prefix_is_removed_from_real_model_name() {
        assert_eq!(
            compact_chip_name("OpenAI · gpt-6-Astra", Some("Codex"), Some("OpenAI")),
            "gpt-6-Astra"
        );
        assert_eq!(
            compact_chip_name("OpenAI · GPT-6.1-Sol", Some("OpenAI"), None),
            "GPT-6.1-Sol"
        );
        assert_eq!(
            compact_chip_name("Gemini 4 Argon (High)", Some("Antigravity"), Some("Google")),
            "Gemini 4 Argon (High)"
        );
        assert_eq!(
            compact_chip_name("Research · Pro", Some("OpenAI"), None),
            "Research · Pro"
        );
        assert_eq!(
            compact_chip_name("OpenAI · Astro", None, None),
            "OpenAI · Astro"
        );
        assert_eq!(
            compact_chip_name("OpenAI · ", Some("OpenAI"), None),
            "OpenAI · "
        );
    }
}

mod picker;
