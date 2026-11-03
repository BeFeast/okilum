//! The remote panel's single navigation hook. LogView owns UI and Back state.
use super::*;
use reader_log::view_stack::Navigation;

impl Panel {
    fn navigate(&mut self, navigation: Navigation, window: &mut Window, cx: &mut Context<Self>) {
        match navigation {
            Navigation::Follow(request) => {
                let (Some(mut source), Some(range)) = (self.applied.clone(), self.range.clone())
                else {
                    return;
                };
                // Freeze the current applied window, keeping KQL and every scope filter.
                source.discover.time = range;
                source.discover.sort = vec![remote_log::discover::Sort {
                    field: source.time_field.clone(),
                    order: "asc".into(),
                }];
                source.discover.filters.push(serde_json::json!({"meta": {"alias": format!("Follow request {}", request.value)},
                    "query": okilum_core::log::correlation::server_query(&request.value, &request.key)}));
                self.choose(source.clone(), window, cx);
                self.requested = Some(source.clone());
                self.busy = true;
                self.message =
                    "Loading request from Kibana in the applied scope and fixed time window…"
                        .into();
                self.generation = self.worker.navigate(source);
            }
            Navigation::Back(loaded) => {
                self.choose(loaded.source.clone(), window, cx);
                self.applied = Some(loaded.source.clone());
                self.range = Some(loaded.range.clone());
                self.data = Some(loaded.window.clone());
                self.quality = Some(loaded.quality.clone());
                // The retained snapshot is exact; closed pagination restarts with Apply.
                self.more = false;
                self.message = "Previous investigation restored. Start follows this query; Apply refreshes history.".into();
                self.generation = self.worker.restore(loaded);
            }
            Navigation::Saved(saved) => {
                let Some(mut source) = saved.remote_source.clone() else {
                    return;
                };
                if !self
                    .sources
                    .iter()
                    .any(|known| known.id == source.id && known.endpoint == source.endpoint)
                {
                    self.error = Some("This view's source is not saved in this vault. Add that source explicitly first.".into());
                    cx.notify();
                    return;
                }
                // Saved query time remains a preset; an explicit remote window is retained separately.
                if let Some(range) = saved
                    .remote_range
                    .clone()
                    .filter(|_| source.discover.time.absolute())
                {
                    source.discover.time = range;
                }
                let key = format!("kibana-{}.jsonl", source.id);
                reader_ui_state::set_log_fields(
                    &key,
                    okilum_core::log::saved_query::FieldChoices {
                        message: saved.message_field.clone(),
                        correlation: saved.correlation_field.clone(),
                    },
                    cx,
                );
                if let Some(columns) = &saved.columns {
                    source.discover.columns = columns.clone();
                    reader_ui_state::set_log_columns(&key, columns.clone(), cx);
                }
                let same_scope = self.applied.as_ref().is_some_and(|previous| {
                    previous.id == source.id && previous.endpoint == source.endpoint
                });
                self.choose(source.clone(), window, cx);
                if same_scope {
                    self.requested = Some(source.clone());
                    self.busy = true;
                    self.message = "Restoring saved investigation from Kibana…".into();
                    self.generation = self.worker.navigate(source);
                } else {
                    self.submit_query(source, false, self.generation, window, cx);
                }
            }
        }
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
                this.navigate(navigation.clone(), window, cx);
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
            view.update(cx, |view, cx| {
                view.remote_context(loaded, self.following, false, cx)
            });
        }
    }
}
