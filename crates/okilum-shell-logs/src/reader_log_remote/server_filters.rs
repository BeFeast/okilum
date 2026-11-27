//! Adapter between generic Log chips and the source's Kibana window.
use super::*;
use okilum_core::{
    log::{
        chips::{
            ColumnCountProvider, ColumnCounts, CountSource, FilterChip, FilterValue, ValueCount,
        },
        query::FieldRef,
        Role,
    },
    remote_log::{
        chips,
        result::{AggregatableField, AuthError, FieldValuesRequest},
    },
};

pub(super) fn named_field(field: &FieldRef, data: &EventWindow) -> Result<FieldRef> {
    if let FieldRef::Key(_) = field {
        return Ok(field.clone());
    }
    let role = match field {
        FieldRef::Time => Role::Time,
        FieldRef::Level => Role::Level,
        FieldRef::Message => Role::Message,
        FieldRef::Logger => Role::Logger,
        _ => anyhow::bail!("This display column has no server field; choose Loaded rows"),
    };
    let key = data.role_field(role).context(
        "This role has no unique source field; choose a named field column or Loaded rows",
    )?;
    Ok(FieldRef::Key(key.into()))
}
pub(super) fn compile(
    chip: &FilterChip,
    catalog: &[AggregatableField],
    data: Option<&EventWindow>,
) -> Result<Option<serde_json::Value>> {
    use okilum_core::log::chips::FilterExecution;
    if !chip.enabled || chip.execution != FilterExecution::Server {
        return Ok(None);
    }
    let mut chip = chip.clone();
    if !matches!(chip.field, FieldRef::Key(_)) {
        chip.field = named_field(
            &chip.field,
            data.context("Load rows before filtering a role")?,
        )?;
    }
    chips::compile(&chip, catalog)
}

pub(super) struct ServerCounts {
    pub client: remote_log::transport::Kibana,
    pub source: Source,
    pub catalog: Result<Vec<AggregatableField>, String>,
    pub data: Arc<EventWindow>,
    pub auth: async_channel::Sender<()>,
}
impl ColumnCountProvider for ServerCounts {
    fn canonical_field(&self, field: &FieldRef) -> FieldRef {
        named_field(field, &self.data).unwrap_or_else(|_| field.clone())
    }
    fn count(&self, field: &FieldRef, _: &[CountSource<'_>]) -> Result<ColumnCounts, String> {
        let result = (|| -> Result<ColumnCounts> {
            let field = named_field(field, &self.data)?;
            let FieldRef::Key(key) = field else {
                unreachable!()
            };
            let catalog = self.catalog.as_ref().map_err(|e| anyhow::anyhow!("{e}"))?;
            let field = chips::mapped_field(&key, catalog)?;
            let result = self.client.field_values(
                &self.source,
                &FieldValuesRequest {
                    field: field.name.clone(),
                    field_type: field.field_type,
                    limit: 1000,
                },
            )?;
            let mut values = result
                .buckets
                .iter()
                .map(|bucket| ValueCount {
                    value: chips::picker_value(&bucket.value),
                    count: usize::try_from(bucket.count).unwrap_or(usize::MAX),
                })
                .collect::<Vec<_>>();
            values.push(ValueCount {
                value: FilterValue::Missing,
                count: usize::try_from(result.missing_count).unwrap_or(usize::MAX),
            });
            let quality = result.status.window_label();
            Ok(ColumnCounts {label:format!("Kibana counts for window · {} → {} · {} · {quality} · top 1,000 · {} docs in other values · error {}",self.source.discover.time.from,self.source.discover.time.to,field.name,result.sum_other_doc_count,result.doc_count_error_upper_bound.map_or("unknown".into(),|n|n.to_string())),values})
        })();
        if result.as_ref().is_err_and(|error| error.is::<AuthError>()) {
            let _ = self.auth.try_send(());
        }
        result.map_err(|error| error.to_string())
    }
}

impl Panel {
    pub(super) fn chips_changed(
        &mut self,
        chips: &[FilterChip],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use okilum_core::log::chips::FilterExecution;
        let server = |chips: &[FilterChip]| {
            chips
                .iter()
                .filter(|chip| chip.enabled && chip.execution == FilterExecution::Server)
                .cloned()
                .collect::<Vec<_>>()
        };
        let changed = server(&self.guided.chips) != server(chips);
        self.guided.chips = chips.to_vec();
        if changed {
            self.apply(window, cx);
        }
    }
    pub(super) fn configure_server_counts(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.view.as_ref() else {
            return;
        };
        let unavailable = if self.busy || self.credential_pending {
            Some("Kibana query is loading; reopen this picker after connecting".to_string())
        } else if let Some(Err(error)) = &self.guided.catalog {
            Some(format!(
                "Kibana field catalog is unavailable: {error}. Refresh to retry"
            ))
        } else if !self.catalog_matches_applied() {
            Some("Kibana catalog does not match the displayed dataset; load the selected source successfully first".to_string())
        } else {
            None
        };
        if let Some(message) = unavailable {
            view.update(cx, |view, cx| {
                view.set_count_provider(Arc::new(CountsUnavailable(message)), cx)
            });
            return;
        }
        let (Some(client), Some(catalog), Some(data), Some(mut source), Some(range)) = (
            self.guided.count_client.clone(),
            self.guided.catalog.clone(),
            self.data.clone(),
            self.applied.clone(),
            self.range.clone(),
        ) else {
            return;
        };
        if self.busy
            || self
                .draft
                .as_ref()
                .is_none_or(|draft| draft.id != source.id || draft.endpoint != source.endpoint)
        {
            return;
        }
        source.discover.time = range;
        let (auth, events) = async_channel::bounded(1);
        let provider = ServerCounts {
            client,
            source,
            catalog,
            data,
            auth,
        };
        view.update(cx, |view, cx| {
            view.set_count_provider(Arc::new(provider), cx)
        });
        let cancelled = self.guided.cancel.clone();
        self.guided._count_auth = Some(cx.spawn(async move |this, cx| {
            while events.recv().await.is_ok() {
                let _ = this.update(cx, |this, cx| {
                    if cancelled.load(std::sync::atomic::Ordering::Acquire) {
                        return;
                    }
                    this.auth_error = true;
                    this.guided.auxiliary_auth_error = true;
                    this.guided.connection = "Auth failed → Replace key".into();
                    cx.notify();
                });
            }
        }));
    }
}

pub(super) struct CountsUnavailable(pub String);
impl ColumnCountProvider for CountsUnavailable {
    fn count(&self, _: &FieldRef, _: &[CountSource<'_>]) -> Result<ColumnCounts, String> {
        Err(self.0.clone())
    }
}
