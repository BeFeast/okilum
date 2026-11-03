//! Guided controls keep user intent separate from the compiled server query.
use super::*;
use gpui_component::{
    calendar::Date,
    date_picker::{DatePicker, DatePickerEvent, DatePickerState},
    menu::{DropdownMenu as _, PopupMenuItem},
};
use remote_log::result::{FieldType, FieldValue, FieldValues, FieldValuesRequest};
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicBool, Ordering},
};

const PRESETS: [(&str, &str); 6] = [
    ("Last 5 min", "now-5m"),
    ("Last 15 min", "now-15m"),
    ("Last 1 h", "now-1h"),
    ("Last 4 h", "now-4h"),
    ("Last 24 h", "now-24h"),
    ("Last 7 d", "now-7d"),
];
const GENERATED: &str = "okilum-guided";

pub(super) struct Guided {
    pub(super) advanced: bool,
    pub(super) adding: bool,
    pub(super) custom: bool,
    pub(super) dates: [Entity<DatePickerState>; 2],
    pub(super) hours: [u32; 2],
    pub(super) minutes: [u32; 2],
    pub(super) _dates: Vec<Subscription>,
    pub(super) scope: BTreeMap<String, FieldValue>,
    pub(super) open_scope: Option<String>,
    pub(super) counts: BTreeMap<String, Result<FieldValues, String>>,
    pub(super) count_client: Option<remote_log::transport::Kibana>,
    pub(super) cancel: Arc<AtomicBool>,
    pub(super) _counts: Option<Task<()>>,
    pub(super) connection: String,
    pub(super) key_needed: bool,
    pub(super) auxiliary_auth_error: bool,
    pub(super) chips: Vec<okilum_core::log::chips::FilterChip>,
    pub(super) _count_auth: Option<Task<()>>,
    pub(super) catalog_scope: Option<String>,
    pub(super) catalog: Option<Result<Vec<remote_log::result::AggregatableField>, String>>,
    pub(super) _catalog: Option<Task<()>>,
    pub(super) discovery: Option<remote_log::result::FieldCatalog>,
    pub(super) catalog_cache: BTreeMap<String, remote_log::result::FieldCatalog>,
    pub(super) details_open: bool,
    #[cfg(test)]
    pub(super) test_key: Option<Vec<u8>>,
}
impl Guided {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Panel>) -> Self {
        let dates = std::array::from_fn(|_| cx.new(|cx| DatePickerState::new(window, cx)));
        let subscriptions = dates
            .iter()
            .map(|date| {
                cx.subscribe_in(date, window, |this, _, event, window, cx| {
                    if matches!(event, DatePickerEvent::Change(_)) {
                        this.set_custom_range(window, cx);
                    }
                })
            })
            .collect();
        Self {
            advanced: false,
            adding: false,
            custom: false,
            dates,
            hours: [0, 0],
            minutes: [0, 0],
            _dates: subscriptions,
            scope: BTreeMap::new(),
            open_scope: None,
            counts: BTreeMap::new(),
            count_client: None,
            cancel: Arc::new(AtomicBool::new(false)),
            _counts: None,
            connection: "Disconnected".into(),
            key_needed: false,
            auxiliary_auth_error: false,
            chips: Vec::new(),
            _count_auth: None,
            catalog: None,
            catalog_scope: None,
            _catalog: None,
            discovery: None,
            catalog_cache: BTreeMap::new(),
            details_open: false,
            #[cfg(test)]
            test_key: None,
        }
    }
    pub(super) fn invalidate_counts(&mut self) {
        self.cancel.store(true, Ordering::Release);
        self.cancel = Arc::new(AtomicBool::new(false));
        self._counts = None;
        self.count_client = None;
        self.counts.clear();
        self.auxiliary_auth_error = false;
        self._count_auth = None;
        self._catalog = None;
        self.catalog = None;
        self.catalog_scope = None;
        self.discovery = None;
    }
}
impl Drop for Guided {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}
impl Panel {
    pub(super) fn guided_candidate(&self, source: &mut Source, cx: &App) -> Result<()> {
        for (field, value) in &self.guided.scope {
            // Existing scope values were selected from confirmed server buckets.
            // Revalidate against this source's cached discovery on every Apply.
            let catalog = self
                .guided
                .catalog
                .as_ref()
                .context("Refresh the source before applying scope")?
                .as_ref()
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let mut selected = source.clone();
            if selected.index_pattern.is_none() {
                selected.index_pattern =
                    self.applied.as_ref().and_then(|s| s.index_pattern.clone());
            }
            anyhow::ensure!(
                self.guided.catalog_scope.as_ref() == Some(&selected.identity_scope()?),
                "Scope mapping does not match this dataset. Clear scope values before applying a changed source"
            );
            let mapping = remote_log::chips::mapped_field(field, catalog)?;
            anyhow::ensure!(
                matches!((value, mapping.field_type),
                    (FieldValue::Missing, _)
                    | (FieldValue::String(_), FieldType::String)
                    | (FieldValue::Number(_), FieldType::Number)
                    | (FieldValue::Boolean(_), FieldType::Boolean)),
                "Scope value type does not match the confirmed field. Clear scope and choose its value again"
            );
            let field = &mapping.name;
            source.discover.filters.push(serde_json::json!({
                "meta":{"controlledBy":GENERATED,"type":"scope","key":field,"value":value_label(value)}, "query":value.filter(field)
            }));
        }
        for chip in &self.guided.chips {
            use okilum_core::log::chips::FilterExecution;
            if !chip.enabled || chip.execution != FilterExecution::Server {
                continue;
            }
            anyhow::ensure!(
                self.catalog_matches_applied()
                    && self.applied.as_ref().is_some_and(|applied| {
                        source.id == applied.id
                            && source.endpoint == applied.endpoint
                            && source.discover.data_view == applied.discover.data_view
                            && source.time_field == applied.time_field
                            && (source.index_pattern.is_none()
                                || source.index_pattern == applied.index_pattern)
                    }),
                "Apply the selected source before using server filters with retained rows"
            );
            let catalog = self
                .guided
                .catalog
                .as_ref()
                .context("Wait for the Kibana field catalog before applying server chips")?
                .as_ref()
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            if let Some(filter) = server_filters::compile(chip, catalog, self.data.as_deref())? {
                source.discover.filters.push(filter);
            }
        }
        if let Some(request) = self
            .view
            .as_ref()
            .and_then(|view| view.read(cx).investigation.request.clone())
        {
            let query = okilum_core::log::correlation::server_query(&request.value, &request.key);
            if !source
                .discover
                .filters
                .iter()
                .any(|filter| filter.get("query") == Some(&query))
            {
                source.discover.filters.push(serde_json::json!({"meta":{"alias":format!("Follow request {}",request.value)},"query":query}));
            }
        }
        Ok(())
    }
    pub(super) fn select_source(
        &mut self,
        source: Source,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.choose(source, window, cx);
        self.guided.adding = false;
        self.apply(window, cx);
    }
    pub(super) fn set_preset(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.guided.custom = false;
        self.from.update(cx, |input, cx| {
            input.set_value(PRESETS[index].1, window, cx)
        });
        self.to
            .update(cx, |input, cx| input.set_value("now", window, cx));
        self.apply(window, cx);
    }
    pub(super) fn open_custom(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.guided.custom = true;
        let range = TimeRange {
            from: self.from.read(cx).value().to_string(),
            to: self.to.read(cx).value().to_string(),
        }
        .resolve(time::OffsetDateTime::now_utc());
        if let Ok(range) = range {
            for (at, text) in [range.from, range.to].iter().enumerate() {
                if let Ok(date) = chrono::DateTime::parse_from_rfc3339(text) {
                    use chrono::Timelike as _;
                    let date = date.with_timezone(&chrono::Utc);
                    self.guided.dates[at].update(cx, |state, cx| {
                        state.set_date(date.date_naive(), window, cx)
                    });
                    self.guided.hours[at] = date.hour();
                    self.guided.minutes[at] = date.minute();
                }
            }
        }
        cx.notify();
    }
    pub(super) fn set_custom_range(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for at in 0..2 {
            let Date::Single(Some(date)) = self.guided.dates[at].read(cx).date() else {
                return;
            };
            let text = format!(
                "{date}T{:02}:{:02}:00Z",
                self.guided.hours[at], self.guided.minutes[at]
            );
            let input = if at == 0 { &self.from } else { &self.to };
            input.update(cx, |input, cx| input.set_value(text, window, cx));
        }
        cx.notify();
    }
    pub(super) fn pin_scope(&mut self, field: String, cx: &mut Context<Self>) {
        let Some(draft) = &mut self.draft else { return };
        if draft.scope_fields.len() >= 16 || draft.scope_fields.contains(&field) {
            return;
        }
        draft.scope_fields.push(field.clone());
        self.persist_definition(cx);
        self.open_scope(field, cx);
    }
    pub(super) fn persist_definition(&mut self, cx: &mut Context<Self>) {
        let Some(source) = self.draft.clone() else {
            return;
        };
        let state = self.state.clone();
        let vault = self.vault.clone();
        cx.spawn(async move |this, cx| {
            let saved = source.clone();
            let result = cx
                .background_executor()
                .spawn(async move { remote_log::save_source(&state, &vault, &saved) })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        if let Some(old) = this.sources.iter_mut().find(|s| s.id == source.id) {
                            *old = source
                        } else {
                            this.sources.push(source)
                        }
                    }
                    Err(error) => this.error = Some(format!("Cannot save scope fields: {error:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }
    pub(super) fn load_catalog(
        &mut self,
        client: remote_log::transport::Kibana,
        source: Source,
        cx: &mut Context<Self>,
    ) {
        let cancelled = self.guided.cancel.clone();
        let cache = self.guided.catalog_cache.clone();
        self.guided._catalog = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let source = client.resolve_source(&source)?;
                    let scope = source.identity_scope()?;
                    let fields = if let Some(cached) = cache.get(&scope) {
                        client.use_cached_catalog(cached);
                        cached.clone()
                    } else {
                        client.field_catalog(&source)?
                    };
                    Ok::<_, anyhow::Error>((scope, fields))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if cancelled.load(Ordering::Acquire) {
                    return;
                }
                if result
                    .as_ref()
                    .is_err_and(|e| e.is::<remote_log::result::AuthError>())
                {
                    this.auth_error = true;
                    this.guided.auxiliary_auth_error = true;
                    this.guided.connection = "Auth failed → Replace key".into();
                }
                this.guided.catalog = Some(match result {
                    Ok((scope, fields)) => {
                        this.guided.catalog_scope = Some(scope.clone());
                        if fields.fallback && !fields.fields.is_empty() {
                            // Bound the source/pattern cache for this window session.
                            if this.guided.catalog_cache.len() >= 16 {
                                this.guided.catalog_cache.pop_first();
                            }
                            this.guided.catalog_cache.insert(scope, fields.clone());
                        }
                        this.guided.discovery = Some(fields.clone());
                        Ok(fields.fields)
                    }
                    Err(error) => {
                        this.guided.catalog_scope = None;
                        Err(error.to_string())
                    }
                });
                this.configure_server_counts(cx);
                cx.notify();
            });
        }));
    }
    pub(super) fn catalog_matches_applied(&self) -> bool {
        self.guided
            .catalog_scope
            .as_ref()
            .zip(self.data.as_ref())
            .is_some_and(|(scope, data)| scope == &data.identity_scope)
    }
    pub(super) fn scalar_type(&self, field: &str) -> Option<FieldType> {
        self.guided
            .catalog
            .as_ref()?
            .as_ref()
            .ok()?
            .iter()
            .find(|info| info.name == field)
            .map(|info| info.field_type)
    }
    pub(super) fn open_scope(&mut self, field: String, cx: &mut Context<Self>) {
        self.guided.open_scope = Some(field.clone());
        if self.guided.counts.contains_key(&field) {
            cx.notify();
            return;
        }
        let Some(client) = self.guided.count_client.clone() else {
            self.guided.counts.insert(
                field,
                Err("Refresh this source before loading server values".into()),
            );
            cx.notify();
            return;
        };
        let Some(mut source) = self.applied.clone().filter(|source| {
            self.draft
                .as_ref()
                .is_some_and(|draft| draft.id == source.id && draft.endpoint == source.endpoint)
                && self.catalog_matches_applied()
        }) else {
            self.guided.counts.insert(
                field,
                Err("Load this source successfully before choosing scope values".into()),
            );
            cx.notify();
            return;
        };
        let Some(field_type) = self.scalar_type(&field) else {
            self.guided.counts.insert(field,Err("Choose an observed scalar field; mixed or structured values cannot be aggregated".into()));
            cx.notify();
            return;
        };
        if let Some(range) = &self.range {
            source.discover.time = range.clone();
        }
        let request = FieldValuesRequest {
            field: field.clone(),
            field_type,
            limit: 1000,
        };
        let generation = self.generation;
        self.guided._counts = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { client.field_values(&source, &request) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                if let Err(error) = &result {
                    if error.is::<remote_log::result::AuthError>() {
                        this.auth_error = true;
                        this.guided.auxiliary_auth_error = true;
                        this.guided.connection = "Auth failed → Replace key".into();
                    }
                }
                this.guided
                    .counts
                    .insert(field, result.map_err(|e| e.to_string()));
                cx.notify();
            });
        }));
        cx.notify();
    }
    pub(super) fn select_scope(
        &mut self,
        field: String,
        value: Option<FieldValue>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.guided.open_scope.as_deref() != Some(&field)
            || self
                .draft
                .as_ref()
                .is_none_or(|source| !source.scope_fields.contains(&field))
        {
            return;
        }
        if let Some(value) = value {
            self.guided.scope.insert(field, value);
        } else {
            self.guided.scope.remove(&field);
        }
        self.guided.open_scope = None;
        self.apply(window, cx);
    }
    // Keep section stack frames bounded; GPUI debug controls are large values.
    pub(super) fn render_guided(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut controls = v_flex().gap_2().child(self.render_environments(cx));
        if self.draft.is_none() {
            return controls
                .child(
                    div()
                        .text_sm()
                        .child("Choose a saved environment or add a source to load its logs."),
                )
                .into_any_element();
        }
        controls = controls.child(self.render_time_controls(cx));
        if self.guided.custom {
            controls = controls.child(self.render_custom_range(cx));
        }
        controls = controls.child(self.render_scope_controls(cx));
        if self.guided.open_scope.is_some() {
            controls = controls.child(self.render_scope_values(cx));
        }
        controls.into_any_element()
    }
    fn render_environments(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut environments = h_flex()
            .gap_2()
            .flex_wrap()
            .child(div().text_sm().child("Environment"));
        for (source_index, source) in self.sources.iter().enumerate() {
            let source = source.clone();
            let id = source.id.to_string();
            environments = environments.child(
                Button::new(SharedString::from(format!("source-{id}")))
                    .debug_selector(move || format!("remote-source-{source_index}"))
                    .small()
                    .label(source.name.clone())
                    .tooltip(format!(
                        "{} · {}",
                        source.endpoint,
                        source
                            .index_pattern
                            .as_deref()
                            .unwrap_or(&source.discover.data_view)
                    ))
                    .selected(self.draft.as_ref().is_some_and(|s| s.id == source.id))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_source(source.clone(), window, cx)
                    })),
            );
        }
        environments = environments.child(
            Button::new("remote-add-source")
                .debug_selector(|| "remote-add-source".into())
                .small()
                .label("Add source")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.guided.adding = !this.guided.adding;
                    cx.notify();
                })),
        );
        environments.into_any_element()
    }
    fn render_time_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut controls = v_flex().gap_2();
        let mut time = h_flex()
            .gap_2()
            .flex_wrap()
            .child(div().text_sm().child("Time · UTC"));
        for (at, (label, from)) in PRESETS.iter().enumerate() {
            let selected = self.from.read(cx).value().as_str() == *from
                && self.to.read(cx).value().as_str() == "now";
            time = time.child(
                Button::new(("remote-time", at))
                    .debug_selector(move || format!("remote-time-{at}"))
                    .small()
                    .label(*label)
                    .selected(selected)
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.set_preset(at, window, cx)),
                    ),
            );
        }
        time = time.child(
            Button::new("remote-custom")
                .debug_selector(|| "remote-custom".into())
                .small()
                .label("Custom…")
                .selected(self.guided.custom)
                .on_click(cx.listener(|this, _, window, cx| this.open_custom(window, cx))),
        );
        controls=controls.child(time).child(div().text_xs().child(if self.to.read(cx).value().as_str()=="now" {
            "Rolling selection · Refresh resolves a new window; loaded results keep their displayed absolute range"
        }else{"Fixed absolute selection · Refresh keeps the same window"}));
        controls.into_any_element()
    }
    fn render_custom_range(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut controls = v_flex().gap_2();
        if self.guided.custom {
            let mut row = h_flex().gap_2();
            for at in 0..2 {
                row = row
                    .child(
                        div()
                            .text_sm()
                            .child(if at == 0 { "From" } else { "To (exclusive)" }),
                    )
                    .child(DatePicker::new(&self.guided.dates[at]).small());
                for (part, value, total) in [
                    (0, self.guided.hours[at], 24),
                    (1, self.guided.minutes[at], 60),
                ] {
                    let weak = cx.entity().downgrade();
                    row = row.child(
                        Button::new(("remote-clock", at * 2 + part))
                            .small()
                            .label(format!("{value:02}"))
                            .tooltip(if part == 0 { "Hour (UTC)" } else { "Minute" })
                            .dropdown_menu(move |mut menu, _, _| {
                                for value in 0..total {
                                    let weak = weak.clone();
                                    menu = menu.item(
                                        PopupMenuItem::new(format!("{value:02}")).on_click(
                                            move |_, window, cx| {
                                                let _ = weak.update(cx, |this, cx| {
                                                    if part == 0 {
                                                        this.guided.hours[at] = value
                                                    } else {
                                                        this.guided.minutes[at] = value
                                                    }
                                                    this.set_custom_range(window, cx);
                                                });
                                            },
                                        ),
                                    );
                                }
                                menu
                            }),
                    );
                }
            }
            controls = controls.child(
                row.child(
                    Button::new("remote-custom-apply")
                        .debug_selector(|| "remote-custom-apply".into())
                        .small()
                        .label("Apply range")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.set_custom_range(window, cx);
                            this.apply(window, cx);
                        })),
                ),
            );
        }
        controls.into_any_element()
    }
    fn render_scope_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut controls = v_flex().gap_2();
        let weak = cx.entity().downgrade();
        let menu_generation = self.generation;
        let fields = self
            .guided
            .catalog
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .map(|fields| {
                fields
                    .iter()
                    .map(|field| field.name.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut scope = h_flex()
            .gap_2()
            .flex_wrap()
            .child(div().text_sm().child("Scope"));
        if let Some(source) = &self.draft {
            for (at, field) in source.scope_fields.iter().enumerate() {
                let field = field.clone();
                let remove = field.clone();
                let value = self
                    .guided
                    .scope
                    .get(&field)
                    .map(value_label)
                    .unwrap_or_else(|| "All values".into());
                scope = scope
                    .child(
                        Button::new(("remote-scope", at))
                            .debug_selector(move || format!("remote-scope-{at}"))
                            .small()
                            .label(format!("{field}: {value}"))
                            .disabled(self.busy || !self.catalog_matches_applied())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_scope(field.clone(), cx)
                            })),
                    )
                    .child(
                        Button::new(("remote-unpin", at))
                            .xsmall()
                            .ghost()
                            .icon(IconName::Close)
                            .tooltip("Unpin scope field")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if let Some(draft) = &mut this.draft {
                                    draft.scope_fields.retain(|f| f != &remove);
                                }
                                let filtered = this.guided.scope.remove(&remove).is_some();
                                this.guided.open_scope = None;
                                this.persist_definition(cx);
                                if filtered {
                                    this.apply(window, cx)
                                } else {
                                    cx.notify();
                                }
                            })),
                    );
            }
        }
        scope = scope.child(
            Button::new("remote-pin-scope")
                .debug_selector(|| "remote-pin-scope".into())
                .small()
                .label("Pin scope field…")
                .disabled(fields.is_empty() || self.busy || !self.catalog_matches_applied())
                .tooltip(
                    "Server confirmed scalar fields; values and counts cover the applied window",
                )
                .dropdown_menu(move |mut menu, _, _| {
                    for field in &fields {
                        let field = field.clone();
                        let weak = weak.clone();
                        menu = menu.item(PopupMenuItem::new(field.clone()).on_click(
                            move |_, _, cx| {
                                let _ = weak.update(cx, |this, cx| {
                                    if this.generation == menu_generation {
                                        this.pin_scope(field.clone(), cx);
                                    }
                                });
                            },
                        ));
                    }
                    menu
                }),
        );
        controls = controls.child(scope);
        if let Some(discovery) = &self.guided.discovery {
            if discovery.fallback {
                controls = controls.child(div().text_xs().child(if discovery.limited {
                    "Field catalog unavailable. Using a bounded search sample and server probes; some fields remain unconfirmed. See Connection details."
                } else {
                    "Field catalog unavailable. Using a search sample and server confirmed fields. See Connection details."
                }));
                let mut observations = v_flex()
                    .gap_1()
                    .id("remote-field-observations")
                    .max_h(px(80.))
                    .overflow_y_scroll();
                for info in &discovery.observed {
                    let aggregatable = discovery.fields.iter().any(|f| f.source_path == info.path);
                    observations = observations.child(div().text_xs().child(format!(
                        "{} ({:?}) · {}",
                        info.path,
                        info.types,
                        if aggregatable {
                            "server confirmed"
                        } else {
                            "not aggregatable — loaded-rows filtering only"
                        }
                    )));
                }
                controls = controls.child(observations);
                if discovery.observed.is_empty() {
                    controls = controls.child(div().text_xs().child(
                        "No fields observed in this window. Widen the time range and Refresh.",
                    ));
                }
            }
        }
        if let Some(Err(error)) = &self.guided.catalog {
            controls = controls.child(div().text_xs().child(format!(
                "Cannot discover aggregatable fields: {error}. Refresh to retry."
            )));
        }
        controls.into_any_element()
    }
    fn render_scope_values(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut controls = v_flex().gap_2();
        if let Some(field) = &self.guided.open_scope {
            let mut picker = v_flex().gap_2().child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .child(format!("{field} · Kibana counts for the applied window")),
                    )
                    .child(
                        Button::new("remote-scope-close")
                            .small()
                            .ghost()
                            .label("Close")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.guided.open_scope = None;
                                cx.notify();
                            })),
                    ),
            );
            let all = field.clone();
            let picker_generation = self.generation;
            picker = picker.child(
                Button::new("remote-scope-all")
                    .debug_selector(|| "remote-scope-all".into())
                    .small()
                    .label("All values")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if this.generation == picker_generation {
                            this.select_scope(all.clone(), None, window, cx)
                        }
                    })),
            );
            match self.guided.counts.get(field) {
                None => picker = picker.child("Loading server values…"),
                Some(Err(error)) => picker = picker.child(format!("Cannot load values: {error}")),
                Some(Ok(counts)) => {
                    picker = picker.child(div().text_xs().child(format!(
                            "{} · Top {} values · {} docs in other values · count error {}",
                            counts.status.window_label(),
                            counts.buckets.len(),
                            counts.sum_other_doc_count,
                            counts
                                .doc_count_error_upper_bound
                                .map_or("unknown".into(), |n| n.to_string())
                        )));
                    let mut values = counts
                        .buckets
                        .iter()
                        .map(|b| (b.value.clone(), b.count))
                        .collect::<Vec<_>>();
                    values.push((FieldValue::Missing, counts.missing_count));
                    let field = field.clone();
                    picker = picker.child(
                        uniform_list(
                            "remote-scope-values",
                            values.len(),
                            cx.processor(move |_, range: std::ops::Range<usize>, _, cx| {
                                range
                                    .map(|at| {
                                        let (value, count) = values[at].clone();
                                        let field = field.clone();
                                        Button::new(("remote-scope-value", at))
                                            .debug_selector(move || {
                                                format!("remote-scope-value-{at}")
                                            })
                                            .small()
                                            .ghost()
                                            .label(format!("{} · {count}", value_label(&value)))
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                if this.generation == picker_generation {
                                                    this.select_scope(
                                                        field.clone(),
                                                        Some(value.clone()),
                                                        window,
                                                        cx,
                                                    )
                                                }
                                            }))
                                            .into_any_element()
                                    })
                                    .collect()
                            }),
                        )
                        .h(px(((counts.buckets.len() + 1).min(6) * 32) as f32)),
                    );
                }
            }
            controls = controls.child(picker);
        }
        controls.into_any_element()
    }
}
pub(super) fn value_label(value: &FieldValue) -> String {
    match value {
        FieldValue::Missing => "Missing".into(),
        _ => value.json().unwrap().to_string(),
    }
}

pub(super) fn utc_label(text: &str) -> String {
    time::OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339)
        .map(|date| date.to_offset(time::UtcOffset::UTC))
        .ok()
        .and_then(|date| {
            date.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .unwrap_or_else(|| text.into())
}
