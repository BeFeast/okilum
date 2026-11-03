//! Shared result quality and picker contracts. No server key is normalized locally.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Number, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TotalRelation {
    Eq,
    Gte,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HitTotal {
    pub value: u64,
    pub relation: TotalRelation,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResultStatus {
    pub total: Option<HitTotal>,
    pub partial: bool,
    pub truncated: bool,
    pub timed_out: bool,
    pub total_shards: Option<u64>,
    pub successful_shards: Option<u64>,
    pub total_clusters: Option<u64>,
    pub successful_clusters: Option<u64>,
    pub failed_shards: u64,
    pub failed_clusters: u64,
    pub skipped_clusters: u64,
    pub partial_clusters: u64,
    pub running_clusters: u64,
    pub metadata_unknown: bool,
    pub version: Option<String>,
}
impl ResultStatus {
    pub fn incomplete(&self) -> bool {
        self.partial
            || self.truncated
            || self.timed_out
            || self.metadata_unknown
            || self.failed_shards > 0
            || self.failed_clusters > 0
            || self.skipped_clusters > 0
            || self.partial_clusters > 0
            || self.running_clusters > 0
    }
    pub fn label(&self, loaded: usize) -> String {
        let mut parts = vec![match &self.total {
            Some(total) => format!(
                "Loaded {loaded} of {}{}",
                if total.relation == TotalRelation::Gte {
                    "at least "
                } else {
                    ""
                },
                total.value
            ),
            None => format!("Loaded {loaded} · total unknown"),
        }];
        self.append_quality(&mut parts);
        parts.join(" · ")
    }
    /// Aggregations cover a window; they do not download event rows.
    pub fn window_label(&self) -> String {
        let total = self.total.as_ref().map_or("unknown".into(), |total| {
            format!(
                "{}{}",
                if total.relation == TotalRelation::Gte {
                    "≥ "
                } else {
                    ""
                },
                total.value
            )
        });
        let mut parts = vec![format!("Window total {total}")];
        self.append_quality(&mut parts);
        parts.join(" · ")
    }
    fn append_quality(&self, parts: &mut Vec<String>) {
        if self.partial {
            parts.push("Partial results".into());
        }
        if self.truncated {
            parts.push("Truncated at the loaded limit".into());
        }
        if self.timed_out {
            parts.push("Search timeout".into());
        }
        if self.failed_shards > 0 {
            parts.push(format!("{} failed shards", self.failed_shards));
        }
        if self.failed_clusters
            + self.skipped_clusters
            + self.partial_clusters
            + self.running_clusters
            > 0
        {
            parts.push(format!(
                "Clusters: {} failed, {} skipped, {} partial, {} running",
                self.failed_clusters,
                self.skipped_clusters,
                self.partial_clusters,
                self.running_clusters
            ));
        }
        if self.metadata_unknown {
            parts.push("Completion metadata unknown".into());
        }
        if let Some(version) = &self.version {
            parts.push(format!("Kibana {version}"));
        }
    }
    pub(crate) fn parse(response: &Value, track: &Value, cross_cluster: bool) -> Result<Self> {
        let raw = response
            .get("rawResponse")
            .context("Malformed Kibana search: rawResponse is missing")?;
        ensure!(
            raw.is_object(),
            "Malformed Kibana search: rawResponse must be an object"
        );
        for key in ["isRunning", "isPartial"] {
            ensure!(
                response.get(key).is_none_or(Value::is_boolean),
                "Malformed Kibana search: invalid {key}"
            );
        }
        let total = match raw.pointer("/hits/total") {
            None | Some(Value::Null) => None,
            Some(value) if value.is_u64() => {
                let value = value.as_u64().unwrap();
                let relation = if track == &Value::Bool(true) {
                    TotalRelation::Eq
                } else if track.as_u64().is_some_and(|cap| value >= cap) {
                    TotalRelation::Gte
                } else if track.is_u64() {
                    TotalRelation::Eq
                } else {
                    anyhow::bail!("Malformed Kibana search: untracked legacy total");
                };
                Some(HitTotal { value, relation })
            }
            Some(value) => Some(HitTotal {
                value: value["value"]
                    .as_u64()
                    .context("Malformed Kibana hit total")?,
                relation: match value["relation"].as_str() {
                    Some("eq") => TotalRelation::Eq,
                    Some("gte") => TotalRelation::Gte,
                    _ => anyhow::bail!("Malformed Kibana hit total relation"),
                },
            }),
        };
        let shards = &raw["_shards"];
        let mut status = Self {
            total,
            partial: response["isPartial"].as_bool().unwrap_or(false),
            timed_out: raw["timed_out"].as_bool().unwrap_or(false),
            failed_shards: shards["failed"].as_u64().unwrap_or(0),
            total_shards: shards["total"].as_u64(),
            successful_shards: shards["successful"].as_u64(),
            metadata_unknown: !raw["timed_out"].is_boolean()
                || shards["failed"].as_u64().is_none()
                || shards["total"].as_u64().is_none()
                || shards["successful"].as_u64().is_none(),
            ..Self::default()
        };
        // skipped shards count among successful shards; a large skipped count is healthy.
        if let (Some(total), Some(successful)) =
            (shards["total"].as_u64(), shards["successful"].as_u64())
        {
            status.partial |= successful < total;
        }
        if let Some(clusters) = raw.get("_clusters") {
            status.total_clusters = clusters["total"].as_u64();
            status.successful_clusters = clusters["successful"].as_u64();
            for (key, target) in [
                ("failed", &mut status.failed_clusters),
                ("skipped", &mut status.skipped_clusters),
                ("partial", &mut status.partial_clusters),
                ("running", &mut status.running_clusters),
            ] {
                if let Some(value) = clusters[key].as_u64() {
                    *target = value;
                } else {
                    status.metadata_unknown = true;
                }
            }
            status.metadata_unknown |=
                clusters["total"].as_u64().is_none() || clusters["successful"].as_u64().is_none();
            status.partial |= clusters["total"].as_u64() != clusters["successful"].as_u64();
            if let Some(details) = clusters["details"].as_object() {
                status.partial |= details.values().any(|c| {
                    c["status"].as_str().is_some_and(|s| s != "successful")
                        || c["timed_out"] == true
                        || c.pointer("/_shards/failed")
                            .and_then(Value::as_u64)
                            .is_some_and(|n| n > 0)
                });
            }
        } else {
            status.metadata_unknown |= cross_cluster;
        }
        Ok(status)
    }
    pub(crate) fn accumulate(&mut self, next: Self) {
        if next.total.is_some() {
            self.total = next.total;
        }
        self.total_shards = next.total_shards.or(self.total_shards);
        self.successful_shards = next.successful_shards.or(self.successful_shards);
        self.total_clusters = next.total_clusters.or(self.total_clusters);
        self.successful_clusters = next.successful_clusters.or(self.successful_clusters);
        self.partial |= next.partial;
        self.truncated |= next.truncated;
        self.timed_out |= next.timed_out;
        self.metadata_unknown |= next.metadata_unknown;
        self.failed_shards = self.failed_shards.max(next.failed_shards);
        self.failed_clusters = self.failed_clusters.max(next.failed_clusters);
        self.skipped_clusters = self.skipped_clusters.max(next.skipped_clusters);
        self.partial_clusters = self.partial_clusters.max(next.partial_clusters);
        self.running_clusters = self.running_clusters.max(next.running_clusters);
    }
}

/// An HTTP or asynchronous completion deadline expired. No credential or URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchTimeout;
impl std::fmt::Display for SearchTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Kibana search timed out; loaded rows are retained. Narrow the range or retry")
    }
}
impl std::error::Error for SearchTimeout {}

/// Typed HTTP 401/403, also used by live::Status::AuthError. Never carries a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthError {
    pub status: u16,
}
impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Kibana authentication or permission denied (HTTP {}); enter a new API key",
            self.status
        )
    }
}
impl std::error::Error for AuthError {}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexPatternRequired;
impl std::fmt::Display for IndexPatternRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Data view cannot be resolved. Enter the index pattern and Apply again; imported query, filters and columns are retained")
    }
}
impl std::error::Error for IndexPatternRequired {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FieldType {
    String,
    Number,
    Boolean,
    Object,
    Array,
    Null,
}
/// A scalar mapping advertised by the catalog or confirmed by server buckets.
/// Text fields use a confirmed keyword multifield, never a guessed suffix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AggregatableField {
    pub name: String,
    pub field_type: FieldType,
    pub source_path: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldInfo {
    pub path: String,
    pub types: BTreeSet<FieldType>,
    pub present_count: usize,
    pub missing_count: usize,
}
/// Flatten _source objects without losing dotted paths, mixed types or missingness.
/// These are observations over loaded rows, not an Elasticsearch mapping claim.
pub fn describe_fields(events: &[Arc<Value>]) -> Vec<FieldInfo> {
    fn visit(value: &Value, path: &str, row: &mut BTreeMap<String, BTreeSet<FieldType>>) {
        let kind = match value {
            Value::String(_) => FieldType::String,
            Value::Number(_) => FieldType::Number,
            Value::Bool(_) => FieldType::Boolean,
            Value::Null => FieldType::Null,
            Value::Array(_) => FieldType::Array,
            Value::Object(_) => FieldType::Object,
        };
        if !path.is_empty() {
            row.entry(path.into()).or_default().insert(kind);
        }
        match value {
            Value::Object(object) => {
                for (key, value) in object {
                    visit(
                        value,
                        &if path.is_empty() {
                            key.clone()
                        } else {
                            format!("{path}.{key}")
                        },
                        row,
                    );
                }
            }
            Value::Array(values) => {
                for value in values {
                    visit(value, path, row);
                }
            }
            _ => {}
        }
    }
    let mut all: BTreeMap<String, FieldInfo> = BTreeMap::new();
    for event in events {
        let mut row = BTreeMap::new();
        if let Some(source) = event.get("_source") {
            visit(source, "", &mut row);
        }
        for (path, types) in row {
            let info = all.entry(path.clone()).or_insert(FieldInfo {
                path,
                types: BTreeSet::new(),
                present_count: 0,
                missing_count: 0,
            });
            info.types.extend(types);
            info.present_count += 1;
        }
    }
    all.into_values()
        .map(|mut info| {
            info.missing_count = events.len() - info.present_count;
            info
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FieldValue {
    String(String),
    Number(Number),
    Boolean(bool),
    Missing,
}
impl FieldValue {
    pub fn json(&self) -> Option<Value> {
        match self {
            Self::String(s) => Some(Value::String(s.clone())),
            Self::Number(n) => Some(Value::Number(n.clone())),
            Self::Boolean(b) => Some(Value::Bool(*b)),
            Self::Missing => None,
        }
    }
    /// Use the exact aggregation key against the aggregated keyword field.
    /// Keyword normalizers apply to term/terms; do not compare to _source text.
    pub fn filter(&self, field: &str) -> Value {
        match self.json() {
            Some(value) => serde_json::json!({"term":{field:value}}),
            None => serde_json::json!({"bool":{"must_not":[{"exists":{"field":field}}]}}),
        }
    }
}
#[derive(Clone, Debug)]
pub struct FieldValuesRequest {
    pub field: String,
    pub limit: usize,
    /// Boolean ES terms use numeric keys and key_as_string; declare the mapping
    /// when querying a boolean field, rather than guessing from source strings.
    pub field_type: FieldType,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldBucket {
    pub value: FieldValue,
    pub count: u64,
}
#[derive(Clone, Debug)]
pub struct FieldValues {
    /// Keys are returned exactly as the server aggregates them (including normalizers).
    pub buckets: Vec<FieldBucket>,
    pub missing_count: u64,
    pub sum_other_doc_count: u64,
    pub doc_count_error_upper_bound: Option<u64>,
    pub status: ResultStatus,
}

/// Session-only observations plus server-confirmed mappings. Unconfirmed paths
/// remain useful for loaded-row filtering but never compile into server chips.
#[derive(Clone, Debug, Default)]
pub struct FieldCatalog {
    pub fields: Vec<AggregatableField>,
    pub observed: Vec<FieldInfo>,
    pub fallback: bool,
    pub limited: bool,
}

/// Allowlisted diagnostics only: never store URLs, patterns, headers, errors,
/// cluster names, aggregation keys or document values here.
#[derive(Clone, Debug, Default)]
pub struct ConnectionDetails {
    pub routes: BTreeMap<String, String>,
    pub version: Option<String>,
    pub status: Option<ResultStatus>,
    pub fallback: Option<&'static str>,
    pub timeouts: bool,
}
impl ConnectionDetails {
    pub(crate) fn route(&mut self, route: &str, outcome: &str) {
        let current = self.routes.entry(route.into()).or_default();
        if !current.split(", ").any(|part| part == outcome) {
            if !current.is_empty() {
                current.push_str(", ");
            }
            current.push_str(outcome);
        }
    }
    pub fn copy_text(&self) -> String {
        let mut lines = vec![
            "Connection details".into(),
            "Endpoint: [redacted]".into(),
            "Pattern: [redacted]".into(),
            format!(
                "Kibana version: {}",
                self.version.as_deref().unwrap_or("not readable")
            ),
        ];
        for route in [
            "version",
            "data view",
            "search",
            "async poll",
            "field catalog",
            "fallback probe",
        ] {
            lines.push(format!(
                "{route}: {}",
                self.routes
                    .get(route)
                    .map(String::as_str)
                    .unwrap_or("not attempted")
            ));
        }
        lines.push(format!(
            "Field discovery: {}",
            self.fallback.unwrap_or("not attempted")
        ));
        if let Some(s) = &self.status {
            lines.push(format!(
                "Total: {}",
                s.total
                    .as_ref()
                    .map(|t| format!(
                        "{} ({})",
                        t.value,
                        if t.relation == TotalRelation::Eq {
                            "eq"
                        } else {
                            "gte"
                        }
                    ))
                    .unwrap_or("unknown".into())
            ));
            lines.push(format!(
                "Shards total/successful: {}/{}; clusters total/successful: {}/{}",
                count_label(s.total_shards),
                count_label(s.successful_shards),
                count_label(s.total_clusters),
                count_label(s.successful_clusters)
            ));
            lines.push(format!("Shards: {} failed; clusters: {} failed, {} skipped, {} partial, {} running; metadata unknown: {}", s.failed_shards, s.failed_clusters, s.skipped_clusters, s.partial_clusters, s.running_clusters, s.metadata_unknown));
            lines.push(format!(
                "Partial: {}; truncated: {}",
                s.partial, s.truncated
            ));
        } else {
            lines.push("Total: unknown; shards/clusters: not readable".into());
        }
        lines.push(format!("Timeouts: {}", self.timeouts));
        lines.join("\n")
    }
}

fn count_label(value: Option<u64>) -> String {
    value.map_or("unknown".into(), |n| n.to_string())
}
