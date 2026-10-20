use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use url::Url;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub language: String,
    pub query: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimeRange {
    pub from: String,
    pub to: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sort {
    pub field: String,
    pub order: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Discover {
    pub data_view: String,
    pub query: Query,
    /// Original filter objects, including disabled metadata and pinning.
    pub filters: Vec<Value>,
    pub columns: Vec<String>,
    pub sort: Vec<Sort>,
    pub time: TimeRange,
    pub refresh_paused: bool,
}

fn keys(value: &Value, allowed: &[&str], context: &str) -> Result<()> {
    let object = value
        .as_object()
        .with_context(|| format!("{context} must be an object"))?;
    for key in object.keys() {
        ensure!(
            allowed.contains(&key.as_str()),
            "Unsupported {context} option: {key}"
        );
    }
    Ok(())
}
fn string(value: &Value, name: &str) -> Result<String> {
    Ok(value[name]
        .as_str()
        .with_context(|| format!("Missing or invalid Discover {name}"))?
        .to_owned())
}

/// Decode form encoding exactly once, then Rison. Never retain the input URL.
pub fn import(input: &str) -> Result<(String, Discover)> {
    ensure!(input.len() <= 131_072, "Discover URL exceeds 128 KiB");
    let url = Url::parse(input).context("Enter a complete HTTPS Discover URL")?;
    ensure!(
        url.scheme() == "https" && url.host_str().is_some(),
        "A Kibana source requires HTTPS"
    );
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "Credentials must be supplied through OS credential storage, not the URL"
    );
    let prefix = url
        .path()
        .strip_suffix("/app/discover")
        .or_else(|| url.path().strip_suffix("/app/discover/"))
        .context("This is not a Discover URL")?;
    let mut endpoint = url.clone();
    endpoint.set_path(prefix);
    endpoint.set_query(None);
    endpoint.set_fragment(None);
    let fragment = url.fragment().unwrap_or("");
    let fragment_query = fragment
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or("");
    let mut app = None;
    let mut global = None;
    for (key, value) in url
        .query_pairs()
        .chain(url::form_urlencoded::parse(fragment_query.as_bytes()))
    {
        match key.as_ref() {
            "_a" => {
                ensure!(app.is_none(), "Duplicate Discover _a state");
                app = Some(super::rison::decode(&value)?);
            }
            "_g" => {
                ensure!(global.is_none(), "Duplicate Discover _g state");
                global = Some(super::rison::decode(&value)?);
            }
            "_tab" => {}
            _ => bail!("Unsupported Discover URL parameter: {key}"),
        }
    }
    let app = app.context("Discover URL must include _a query state")?;
    let global = global.unwrap_or_else(|| json!({}));
    keys(
        &app,
        &[
            "dataSource",
            "index",
            "query",
            "filters",
            "columns",
            "sort",
            "hideChart",
            "interval",
            "grid",
            "rowHeight",
            "viewMode",
            "breakdownField",
            "hideAggregatedPreview",
            "sampleSize",
        ],
        "application state",
    )?;
    // Options that change the represented data cannot be silently treated as presentation.
    for key in ["breakdownField", "sampleSize"] {
        ensure!(
            app.get(key).is_none_or(Value::is_null),
            "Unsupported Discover semantic option: {key}"
        );
    }
    keys(
        &global,
        &["time", "refreshInterval", "filters"],
        "global state",
    )?;
    let data_view = if let Some(source) = app.get("dataSource") {
        keys(source, &["dataViewId", "type"], "data source")?;
        ensure!(
            source["type"] == "dataView",
            "Only Discover data-view sources are supported"
        );
        let id = string(source, "dataViewId")?;
        ensure!(
            app.get("index")
                .is_none_or(|old| old == &Value::String(id.clone())),
            "Conflicting data-view identities"
        );
        id
    } else {
        string(&app, "index")?
    };
    let query: Query = serde_json::from_value(
        app.get("query")
            .cloned()
            .unwrap_or_else(|| json!({"language":"kuery", "query":""})),
    )
    .context("Unsupported Discover query state")?;
    let mut filters = Vec::new();
    for container in [&global, &app] {
        if let Some(value) = container.get("filters") {
            filters.extend(
                value
                    .as_array()
                    .context("Discover filters must be an array")?
                    .iter()
                    .cloned(),
            );
        }
    }
    let columns = serde_json::from_value(app.get("columns").cloned().unwrap_or_else(|| json!([])))
        .context("Discover columns must be strings")?;
    let sort = match app.get("sort") {
        None => vec![Sort {
            field: "@timestamp".into(),
            order: "desc".into(),
        }],
        Some(value) => value
            .as_array()
            .context("Discover sort must be an array")?
            .iter()
            .map(|pair| {
                let pair = pair
                    .as_array()
                    .context("Unsupported Discover sort expression")?;
                ensure!(pair.len() == 2, "Unsupported Discover sort expression");
                Ok(Sort {
                    field: pair[0].as_str().context("Invalid sort field")?.into(),
                    order: pair[1].as_str().context("Invalid sort direction")?.into(),
                })
            })
            .collect::<Result<Vec<_>>>()?,
    };
    let time = match global.get("time") {
        Some(value) => {
            keys(value, &["from", "to", "mode"], "time range")?;
            TimeRange {
                from: string(value, "from")?,
                to: string(value, "to")?,
            }
        }
        None => TimeRange {
            from: "now-15m".into(),
            to: "now".into(),
        },
    };
    let refresh_paused = match global.get("refreshInterval") {
        None => true,
        Some(value) => {
            keys(value, &["pause", "value"], "refresh interval")?;
            ensure!(
                value["value"].as_u64().is_some(),
                "Invalid Discover refresh interval"
            );
            value["pause"]
                .as_bool()
                .context("Invalid Discover refresh pause")?
        }
    };
    let state = Discover {
        data_view,
        query,
        filters,
        columns,
        sort,
        time,
        refresh_paused,
    };
    state.validate()?;
    Ok((endpoint.as_str().trim_end_matches('/').into(), state))
}

impl Discover {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.data_view.is_empty() && self.data_view.len() <= 1024,
            "Invalid data-view identity"
        );
        ensure!(
            self.query.language == "kuery",
            "Only KQL (kuery) is supported; no Lucene substitution is made"
        );
        ensure!(self.query.query.len() <= 16_384, "KQL exceeds 16 KiB");
        ensure!(
            !self.sort.is_empty() && self.sort.len() <= 8,
            "Choose one to eight sort fields"
        );
        for sort in &self.sort {
            ensure!(
                !sort.field.is_empty()
                    && sort.field.len() <= 256
                    && matches!(sort.order.as_str(), "asc" | "desc"),
                "Unsupported sort field or direction"
            );
            ensure!(
                sort.field != "_shard_doc",
                "Discover must not supply its own PIT tie-breaker"
            );
        }
        ensure!(
            self.filters.len() <= 100 && serde_json::to_vec(&self.filters)?.len() <= 65_536,
            "Filters exceed the supported size"
        );
        self.filter_clauses()?;
        self.time.resolve(time::OffsetDateTime::now_utc())?;
        Ok(())
    }
    pub fn filter_clauses(&self) -> Result<(Vec<Value>, Vec<Value>)> {
        let mut yes = Vec::new();
        let mut no = Vec::new();
        for filter in &self.filters {
            keys(
                filter,
                &["meta", "$state", "query", "range", "exists"],
                "filter",
            )?;
            if let Some(state) = filter.get("$state") {
                keys(state, &["store"], "filter pinning")?;
                ensure!(
                    matches!(state["store"].as_str(), Some("appState" | "globalState")),
                    "Unsupported filter pinning"
                );
            }
            let meta = filter.get("meta").cloned().unwrap_or_else(|| json!({}));
            keys(
                &meta,
                &[
                    "alias",
                    "disabled",
                    "negate",
                    "index",
                    "key",
                    "value",
                    "params",
                    "type",
                    "field",
                    "controlledBy",
                ],
                "filter metadata",
            )?;
            for key in ["disabled", "negate"] {
                ensure!(
                    meta.get(key).is_none_or(Value::is_boolean),
                    "Invalid filter {key} flag"
                );
            }
            if meta["disabled"].as_bool() == Some(true) {
                continue;
            }
            ensure!(
                meta.get("index")
                    .is_none_or(|id| id.is_null() || id.as_str() == Some(&self.data_view)),
                "Active filter references a different data view"
            );
            let clause = if let Some(query) = filter.get("query") {
                query.clone()
            } else if let Some(range) = filter.get("range") {
                json!({"range":range})
            } else if let Some(exists) = filter.get("exists") {
                json!({"exists":exists})
            } else {
                bail!("Enabled filter has no supported query clause");
            };
            validate_clause(&clause, 0)?;
            if meta["negate"].as_bool() == Some(true) {
                no.push(clause);
            } else {
                yes.push(clause);
            }
        }
        Ok((yes, no))
    }
    pub fn search_body(
        &self,
        range: &TimeRange,
        after: Option<&Value>,
        size: usize,
    ) -> Result<Value> {
        self.validate()?;
        let (mut filter, must_not) = self.filter_clauses()?;
        filter.push(json!({"range":{"@timestamp":{"gte":range.from,"lte":range.to,"format":"strict_date_optional_time_nanos"}}}));
        if !self.query.query.trim().is_empty() {
            filter.push(json!({"kql":{"query":self.query.query}}));
        }
        let mut sort: Vec<_> = self.sort.iter().map(|sort| {
            if sort.field == "@timestamp" { json!({"@timestamp":{"order":sort.order,"format":"strict_date_optional_time_nanos","numeric_type":"date_nanos"}}) }
            else { json!({sort.field.clone():{"order":sort.order}}) }
        }).collect();
        sort.push(json!({"_shard_doc": "asc"}));
        let mut body = json!({"size":size,"_source":true,"fields":[{"field":"*","include_unmapped":true},{"field":"@timestamp","format":"strict_date_optional_time_nanos"}],"query":{"bool":{"filter":filter,"must_not":must_not}},"sort":sort,"track_total_hits":false,"timeout":"5s"});
        if let Some(after) = after {
            body["search_after"] = after.clone();
        }
        Ok(body)
    }
}

fn validate_clause(value: &Value, depth: usize) -> Result<()> {
    ensure!(depth < 16, "Filter is nested too deeply");
    let object = value
        .as_object()
        .context("Filter clause must be an object")?;
    ensure!(object.len() == 1, "Unsupported compound filter clause");
    let (kind, body) = object.iter().next().unwrap();
    ensure!(
        [
            "match_phrase",
            "match",
            "term",
            "terms",
            "range",
            "exists",
            "bool",
            "match_all",
            "wildcard"
        ]
        .contains(&kind.as_str()),
        "Unsupported filter query: {kind}"
    );
    if kind == "bool" {
        keys(
            body,
            &[
                "filter",
                "must",
                "must_not",
                "should",
                "minimum_should_match",
            ],
            "boolean filter",
        )?;
        for key in ["filter", "must", "must_not", "should"] {
            if let Some(clauses) = body.get(key) {
                if let Some(clauses) = clauses.as_array() {
                    for clause in clauses {
                        validate_clause(clause, depth + 1)?;
                    }
                } else {
                    validate_clause(clauses, depth + 1)?;
                }
            }
        }
    }
    Ok(())
}

impl TimeRange {
    pub fn absolute(&self) -> bool {
        !self.from.starts_with("now") && !self.to.starts_with("now")
    }
    pub fn resolve(&self, now: time::OffsetDateTime) -> Result<Self> {
        fn parse(text: &str, now: time::OffsetDateTime) -> Result<time::OffsetDateTime> {
            if !text.starts_with("now") {
                return time::OffsetDateTime::parse(
                    text,
                    &time::format_description::well_known::Rfc3339,
                )
                .context("Unsupported absolute time; use RFC3339 with a timezone");
            }
            if text == "now" {
                return Ok(now);
            }
            let re = regex::Regex::new(r"^now-(\d+)(ms|s|m|h|d|w)(?:/([smhd]))?$").unwrap();
            let c = re.captures(text).context(
                "Unsupported date math; use now or now-Nms/s/m/h/d/w, optionally rounded /s/m/h/d",
            )?;
            let scale = |unit: &str| -> i128 {
                match unit {
                    "ms" => 1_000_000,
                    "s" => 1_000_000_000,
                    "m" => 60_000_000_000,
                    "h" => 3_600_000_000_000,
                    "d" => 86_400_000_000_000,
                    _ => 604_800_000_000_000,
                }
            };
            let n: i128 = c[1].parse().context("Time range is too large")?;
            let offset = n
                .checked_mul(scale(&c[2]))
                .context("Time range is too large")?;
            let mut stamp = now
                .unix_timestamp_nanos()
                .checked_sub(offset)
                .context("Time range is too large")?;
            if let Some(unit) = c.get(3) {
                stamp -= stamp.rem_euclid(scale(unit.as_str()));
            }
            time::OffsetDateTime::from_unix_timestamp_nanos(stamp)
                .context("Time range is outside the supported dates")
        }
        let from = parse(&self.from, now)?;
        let to = parse(&self.to, now)?;
        ensure!(from < to, "Time range must end after it starts");
        Ok(Self {
            from: from.format(&time::format_description::well_known::Rfc3339)?,
            to: to.format(&time::format_description::well_known::Rfc3339)?,
        })
    }
}
