//! Search-only discovery. Sampling observes types; only successful nonempty
//! terms buckets confirm a mapping. A failed combined aggregation is bisected
//! because Elasticsearch rejects the whole request for one text-only field.
use super::*;
use crate::remote_log::result::{describe_fields, AggregatableField, FieldCatalog};
use std::{collections::BTreeSet, time::Instant};

const SAMPLE_ROWS: usize = 128;
const CANDIDATE_FIELDS: usize = 24;
const PROBE_REQUESTS: usize = 64;
const DISCOVERY_BUDGET: Duration = Duration::from_secs(8);

impl Kibana {
    /// A source/pattern-scoped UI session cache avoids repeating bounded probes.
    pub fn use_cached_catalog(&self, catalog: &FieldCatalog) {
        self.diagnostics.lock().unwrap().fallback = Some(if catalog.limited {
            "cached search sample + terms probes (bounded; some fields unconfirmed)"
        } else {
            "cached search sample + terms probes"
        });
    }
    pub fn aggregatable_fields(&self, source: &Source) -> Result<Vec<AggregatableField>> {
        Ok(self.field_catalog(source)?.fields)
    }

    pub fn field_catalog(&self, source: &Source) -> Result<FieldCatalog> {
        let source = self.resolve_source(source)?;
        match self.mapped_fields(&source) {
            Ok(fields) => {
                self.diagnostics.lock().unwrap().fallback =
                    Some("field catalog (fallback not used)");
                return Ok(FieldCatalog {
                    fields,
                    ..Default::default()
                });
            }
            Err(_) if !self.cancelled.load(Ordering::Acquire) => {
                self.diagnostics
                    .lock()
                    .unwrap()
                    .route("field catalog", "unavailable, malformed or empty");
            }
            Err(error) => return Err(error),
        }
        self.diagnostics.lock().unwrap().fallback = Some("search sample + terms probes");
        let began = Instant::now();
        let range = source
            .discover
            .time
            .resolve(time::OffsetDateTime::now_utc())?;
        let mut params =
            source
                .discover
                .search_body_for(&range, None, SAMPLE_ROWS, &source.time_field)?;
        params["index"] = json!(source.index_pattern.as_ref().unwrap());
        let (response, timeout) = self.search(&params, DISCOVERY_BUDGET)?;
        ensure!(
            !timeout,
            "Field sampling timed out; narrow the window and Refresh"
        );
        let rows = response
            .pointer("/rawResponse/hits/hits")
            .and_then(Value::as_array)
            .context("Malformed field sample")?;
        ensure!(
            rows.len() <= SAMPLE_ROWS,
            "Field sample exceeded the row bound"
        );
        let events = rows.iter().cloned().map(Arc::new).collect::<Vec<_>>();
        let mut observed = describe_fields(&events);
        let observations_limited = observed.len() > 256;
        observed.truncate(256);
        let mut candidates = Vec::new();
        let mut path_count = 0;
        for info in &observed {
            if info.path.len() > 248 || info.types.contains(&FieldType::Object) {
                continue;
            }
            let scalar = info
                .types
                .iter()
                .copied()
                .filter(|t| {
                    matches!(
                        t,
                        FieldType::String | FieldType::Number | FieldType::Boolean
                    )
                })
                .collect::<BTreeSet<_>>();
            if scalar.len() != 1 {
                continue;
            }
            path_count += 1;
            if path_count > CANDIDATE_FIELDS {
                continue;
            }
            let field_type = *scalar.first().unwrap();
            candidates.push(AggregatableField {
                name: info.path.clone(),
                source_path: info.path.clone(),
                field_type,
            });
            if field_type == FieldType::String {
                candidates.push(AggregatableField {
                    name: format!("{}.keyword", info.path),
                    source_path: info.path.clone(),
                    field_type,
                });
            }
        }
        params["size"] = json!(0);
        params.as_object_mut().unwrap().remove("sort");
        let mut confirmed = Vec::new();
        let mut requests = 0;
        let mut limited = observations_limited || path_count > CANDIDATE_FIELDS;
        for batch in candidates.chunks(8) {
            self.probe_fields(
                &params,
                batch,
                began,
                &mut requests,
                &mut confirmed,
                &mut limited,
            )?;
        }
        // Prefer an exact scalar mapping when both base and multifield work.
        let exact: BTreeSet<_> = confirmed
            .iter()
            .filter(|f| f.name == f.source_path)
            .map(|f| f.name.clone())
            .collect();
        confirmed.retain(|f| f.name == f.source_path || !exact.contains(&f.source_path));
        confirmed.sort_by(|a, b| a.name.cmp(&b.name));
        self.diagnostics.lock().unwrap().fallback = Some(if limited {
            "search sample + terms probes (bounded; some fields unconfirmed)"
        } else {
            "search sample + terms probes"
        });
        Ok(FieldCatalog {
            fields: confirmed,
            observed,
            fallback: true,
            limited,
        })
    }

    pub(super) fn probe_fields(
        &self,
        base: &Value,
        fields: &[AggregatableField],
        began: Instant,
        requests: &mut usize,
        confirmed: &mut Vec<AggregatableField>,
        limited: &mut bool,
    ) -> Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            bail!("Log request cancelled");
        }
        if *requests >= PROBE_REQUESTS || began.elapsed() >= DISCOVERY_BUDGET {
            *limited = true;
            return Ok(());
        }
        let mut params = base.clone();
        let mut aggs = serde_json::Map::new();
        for (i, field) in fields.iter().enumerate() {
            aggs.insert(
                format!("probe_{i}"),
                json!({"terms":{"field":field.name,"size":1}}),
            );
        }
        params["aggs"] = Value::Object(aggs);
        *requests += 1;
        match self.search(&params, DISCOVERY_BUDGET.saturating_sub(began.elapsed())) {
            Ok((response, false)) => {
                let status = ResultStatus::parse(
                    &response,
                    &params["track_total_hits"],
                    base["index"].as_str().is_some_and(|p| p.contains(':')),
                )?;
                // Missing completion metadata is reported, but does not turn
                // successful buckets into a guessed mapping. Explicit partial
                // failures/timeouts still cannot confirm a field across indices.
                *limited |= status.metadata_unknown;
                let mut quality = status;
                quality.metadata_unknown = false;
                if quality.incomplete() {
                    *limited = true;
                    return Ok(());
                }
                for (i, field) in fields.iter().enumerate() {
                    if response["rawResponse"]["aggregations"][format!("probe_{i}")]["buckets"]
                        .as_array()
                        .is_some_and(|b| {
                            !b.is_empty()
                                && b.iter().all(|v| match field.field_type {
                                    FieldType::String => v["key"].is_string(),
                                    FieldType::Number => v["key"].is_number(),
                                    FieldType::Boolean => {
                                        v["key"].is_boolean()
                                            || matches!(
                                                v["key_as_string"].as_str(),
                                                Some("true" | "false")
                                            )
                                    }
                                    _ => false,
                                })
                        })
                    {
                        confirmed.push(field.clone());
                    }
                }
            }
            Ok((_, true)) => *limited = true,
            Err(error)
                if error.is::<AuthError>()
                    || error.is::<Transient>()
                    || error.is::<SearchTimeout>() =>
            {
                *limited = true;
            }
            Err(_) if fields.len() > 1 => {
                let middle = fields.len() / 2;
                self.probe_fields(base, &fields[..middle], began, requests, confirmed, limited)?;
                self.probe_fields(base, &fields[middle..], began, requests, confirmed, limited)?;
            }
            Err(_) => {}
        }
        Ok(())
    }
}
