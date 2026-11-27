//! Navigation owns one worker command; guided selections never contain compiled filters.
use super::*;
use okilum_core::log::saved_query::RemoteGuided;
use reader_log::view_stack::Navigation;

impl Panel {
    pub(super) fn guided_context(&self) -> Option<RemoteGuided> {
        Some(RemoteGuided {
            source: self
                .applied_definition
                .clone()
                .or_else(|| self.applied.clone())?,
            scope: self.guided.scope.clone(),
        })
    }

    fn choose_navigation(
        &mut self,
        source: Source,
        scope: std::collections::BTreeMap<String, remote_log::result::FieldValue>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let same = self
            .applied
            .as_ref()
            .is_some_and(|old| old.id == source.id && old.endpoint == source.endpoint);
        let auxiliary_auth_error = self.guided.auxiliary_auth_error;
        let client = same.then(|| self.guided.count_client.clone()).flatten();
        let catalog = same.then(|| self.guided.catalog.clone()).flatten();
        let catalog_scope = same.then(|| self.guided.catalog_scope.clone()).flatten();
        let state = self.view.as_ref().map(|view| {
            let view = view.read(cx);
            (view.filter_view_state(), view.investigation.request.clone())
        });
        self.choose(source, window, cx);
        self.guided.scope = scope;
        if let Some(client) = client {
            self.guided.count_client = Some(client.with_cancellation(self.guided.cancel.clone()));
            self.guided.catalog = catalog;
            self.guided.catalog_scope = catalog_scope;
            self.guided.auxiliary_auth_error = auxiliary_auth_error;
            self.auth_error = auxiliary_auth_error;
        }
        if let (Some(view), Some((filters, request))) = (&self.view, state) {
            self.guided.chips = view.update(cx, |view, cx| {
                view.investigation.request = request;
                view.filter_ui.navigating = true;
                view.restore_filter_view(filters, window, cx);
                view.filter_ui.navigating = false;
                view.filter_chips().to_vec()
            });
        }
    }

    fn navigate(&mut self, navigation: Navigation, window: &mut Window, cx: &mut Context<Self>) {
        match navigation {
            Navigation::Follow(request) => {
                if let Some(view) = &self.view {
                    view.update(cx, |view, _| view.investigation.request = Some(request));
                }
                let (Some(context), Some(range)) = (self.guided_context(), self.range.clone())
                else {
                    return;
                };
                let mut base = context.source;
                // Table chips belong to Back. The child keeps the environment,
                // opaque query and pinned scope, and traverses the full applied window.
                base.discover.sort = vec![remote_log::discover::Sort {
                    field: base.time_field.clone(),
                    order: "asc".into(),
                }];
                self.choose_navigation(base.clone(), context.scope, window, cx);
                let mut source = match self.candidate(cx) {
                    Ok(source) => source,
                    Err(error) => {
                        self.error = Some(error.to_string());
                        cx.notify();
                        return;
                    }
                };
                source.discover.time = range;
                self.requested_definition = Some(base);
                self.requested = Some(source.clone());
                self.busy = true;
                self.message =
                    "Loading request from Kibana in the applied scope and fixed time window…"
                        .into();
                self.generation = self.worker.navigate(source);
            }
            Navigation::Back(loaded, context) => {
                let context = context
                    .map(|context| *context)
                    .unwrap_or_else(|| RemoteGuided {
                        source: loaded.source.clone(),
                        scope: Default::default(),
                    });
                self.choose_navigation(context.source.clone(), context.scope, window, cx);
                self.applied_definition = Some(context.source.clone());
                self.requested_definition = Some(context.source);
                self.requested = Some(loaded.source.clone());
                self.applied = Some(loaded.source.clone());
                self.range = Some(loaded.range.clone());
                self.data = Some(loaded.window.clone());
                self.quality = Some(loaded.quality.clone());
                self.more = false;
                self.guided.connection = "Connected · restored snapshot".into();
                self.message = "Previous investigation restored. Start follows this query; Refresh re-queries history.".into();
                self.generation = self.worker.restore(loaded);
            }
            Navigation::Saved(saved) => {
                let Some(mut compiled) = saved.remote_source.clone() else {
                    return;
                };
                if !self
                    .sources
                    .iter()
                    .any(|known| known.id == compiled.id && known.endpoint == compiled.endpoint)
                {
                    self.error = Some("This view's source is not saved in this vault. Add that source explicitly first.".into());
                    cx.notify();
                    return;
                }
                let mut context = saved.remote_guided.clone().unwrap_or_else(|| RemoteGuided {
                    source: compiled.clone(),
                    scope: Default::default(),
                });
                // Keep the rolling preset in the base definition; fixed saved views
                // retain their exact applied window. Legacy views keep their full DSL.
                if let Some(range) = saved
                    .remote_range
                    .clone()
                    .filter(|_| context.source.discover.time.absolute())
                {
                    context.source.discover.time = range;
                }
                compiled.discover.time = context.source.discover.time.clone();
                let key = format!("kibana-{}.jsonl", compiled.id);
                reader_ui_state::set_log_fields(
                    &key,
                    okilum_core::log::saved_query::FieldChoices {
                        message: saved.message_field.clone(),
                        correlation: saved.correlation_field.clone(),
                    },
                    cx,
                );
                if let Some(columns) = &saved.columns {
                    compiled.discover.columns = columns.clone();
                    context.source.discover.columns = columns.clone();
                    reader_ui_state::set_log_columns(&key, columns.clone(), cx);
                }
                let same = self
                    .applied
                    .as_ref()
                    .is_some_and(|old| old.id == compiled.id && old.endpoint == compiled.endpoint);
                self.choose_navigation(context.source.clone(), context.scope, window, cx);
                self.guided.chips = saved.filter_chips.clone();
                self.requested_definition = Some(context.source);
                if same {
                    self.requested = Some(compiled.clone());
                    self.busy = true;
                    self.message = "Restoring saved investigation from Kibana…".into();
                    if let Some(client) = self.guided.count_client.clone() {
                        self.guided.catalog = None;
                        self.guided.catalog_scope = None;
                        self.load_catalog(client, compiled.clone(), cx);
                    }
                    self.generation = self.worker.navigate(compiled);
                } else {
                    self.submit_query(compiled, false, false, self.generation, window, cx);
                }
            }
        }
        self.configure_server_counts(cx);
        cx.notify();
    }

    pub(super) fn install_investigation(
        &mut self,
        view: &Entity<reader_log::LogView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self._navigation = Some(cx.subscribe_in(
            view,
            window,
            |this, _, navigation: &Navigation, window, cx| {
                this.navigate(navigation.clone(), window, cx)
            },
        ));
        if let (Some(source), Some(range), Some(data)) = (&self.applied, &self.range, &self.data) {
            let loaded = Arc::new(remote_log::live::Loaded {
                source: source.clone(),
                range: range.clone(),
                window: data.clone(),
                quality: self.quality.clone().unwrap_or_default(),
                more: self.more,
            });
            let guided = self.guided_context();
            view.update(cx, |view, cx| {
                view.remote_context(loaded, self.following, false, cx);
                view.investigation.guided = guided;
            });
        }
    }
}
