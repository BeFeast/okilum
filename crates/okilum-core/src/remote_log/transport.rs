//! Capability-checked Kibana internal-search transport. No redirects, secret diagnostics, or disk
//! event cache. A history session must be owned by a background worker.
use super::{
    discover::{Discover, TimeRange},
    result::{
        AuthError, FieldBucket, FieldType, FieldValue, FieldValues, FieldValuesRequest,
        IndexPatternRequired, ResultStatus, SearchTimeout,
    },
    Source, MAX_EVENT_BYTES, MAX_ROWS, PAGE_ROWS,
};
use anyhow::{bail, ensure, Context, Result};
use reqwest::{Client, Method};
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use url::Url;

#[path = "catalog.rs"]
mod catalog;

const RESPONSE_BYTES: u64 = 8 * 1024 * 1024;

/// Unit-test clock: advancing time notifies active HTTP deadlines without sleeps.
#[cfg(test)]
pub(super) struct TestClock {
    now: Mutex<std::time::Instant>,
    waiters: Mutex<Vec<async_channel::Sender<()>>>,
    expired: std::sync::atomic::AtomicUsize,
}
#[cfg(test)]
impl TestClock {
    pub(super) fn new() -> Self {
        Self {
            now: Mutex::new(std::time::Instant::now()),
            waiters: Default::default(),
            expired: Default::default(),
        }
    }
    pub(super) fn now(&self) -> std::time::Instant {
        *self.now.lock().unwrap()
    }
    pub(super) fn advance(&self, duration: Duration) {
        *self.now.lock().unwrap() += duration;
        self.waiters
            .lock()
            .unwrap()
            .retain(|waiter| waiter.force_send(()).is_ok());
    }
    pub(super) fn expirations(&self) -> usize {
        self.expired.load(Ordering::Acquire)
    }
    async fn deadline(&self, began: std::time::Instant, budget: Duration) {
        let (sender, receiver) = async_channel::bounded(1);
        {
            let mut waiters = self.waiters.lock().unwrap();
            waiters.retain(|waiter| !waiter.is_closed());
            waiters.push(sender);
        }
        loop {
            if self.now().duration_since(began) >= budget {
                self.expired.fetch_add(1, Ordering::AcqRel);
                return;
            }
            // Register before checking time; a concurrent advance is buffered.
            receiver.recv().await.unwrap();
        }
    }
}

#[derive(Clone)]
pub struct ApiKey(zeroize::Zeroizing<Vec<u8>>);
impl ApiKey {
    pub fn new(bytes: Vec<u8>) -> Result<Self> {
        let bytes = zeroize::Zeroizing::new(bytes);
        ensure!(
            !bytes.is_empty() && bytes.len() <= 8192 && bytes.iter().all(|b| b.is_ascii_graphic()),
            "Enter an encoded Kibana API key without whitespace"
        );
        Ok(Self(bytes))
    }
    /// Borrow only for the explicit OS credential write; never serialize this buffer.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}
impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey([redacted])")
    }
}
/// Only explicit transport outages retry automatically; query/auth/quality errors do not.
#[derive(Debug)]
pub(super) struct Transient(pub &'static str);
impl std::fmt::Display for Transient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for Transient {}

#[derive(Debug)]
struct Rejected(String);
impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Rejected {}

#[derive(Clone)]
pub struct Kibana {
    client: Client,
    endpoint: Url,
    source_id: Option<uuid::Uuid>,
    key: Arc<ApiKey>,
    runtime: Arc<tokio::runtime::Runtime>,
    cancelled: Arc<AtomicBool>,
    diagnostics: Arc<Mutex<super::result::ConnectionDetails>>,
    #[cfg(test)]
    test_clock: Option<Arc<TestClock>>,
    #[cfg(test)]
    polls_started: Arc<std::sync::atomic::AtomicUsize>,
}
impl Kibana {
    pub fn new(source: &Source, key: ApiKey, cancelled: Arc<AtomicBool>) -> Result<Self> {
        source.validate()?;
        let mut kibana = Self::with_endpoint(Url::parse(&source.endpoint)?, key, cancelled)?;
        kibana.source_id = Some(source.id);
        Ok(kibana)
    }
    pub(super) fn with_endpoint(
        endpoint: Url,
        key: ApiKey,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Self> {
        let runtime = Arc::new(
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?,
        );
        let client = okilum_http::async_builder(
            endpoint.as_str(),
            env!("CARGO_PKG_VERSION"),
            Duration::from_secs(10),
        )
        // Proxy/PAC diagnostics can contain URLs: keep source details and
        // credentials out of the UI error chain.
        .map_err(|error| anyhow::anyhow!("Kibana network policy: {}", okilum_http::reason(&error)))?
        .connect_timeout(Duration::from_secs(3))
        .build()
        .map_err(|error| {
            anyhow::anyhow!("Kibana HTTP client: {}", okilum_http::reason(&error.into()))
        })?;
        Ok(Self {
            client,
            endpoint,
            source_id: None,
            key: Arc::new(key),
            runtime,
            cancelled,
            diagnostics: Default::default(),
            #[cfg(test)]
            test_clock: None,
            #[cfg(test)]
            polls_started: Default::default(),
        })
    }
    #[cfg(test)]
    pub(super) fn with_test_clock(mut self, clock: Arc<TestClock>) -> Result<Self> {
        let loopback = match self.endpoint.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            _ => false,
        };
        ensure!(
            self.endpoint.scheme() == "http" && loopback,
            "A test clock requires a synthetic HTTP loopback endpoint"
        );
        // Replace reqwest's real-time timers with the same budgets on our clock.
        // This client cannot address a real source or forward credentials away.
        self.client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        self.test_clock = Some(clock);
        Ok(self)
    }
    fn now(&self) -> std::time::Instant {
        #[cfg(test)]
        if let Some(clock) = &self.test_clock {
            return clock.now();
        }
        std::time::Instant::now()
    }
    pub fn with_diagnostics(
        mut self,
        details: Arc<Mutex<super::result::ConnectionDetails>>,
    ) -> Self {
        self.diagnostics = details;
        self
    }
    pub fn connection_details(&self) -> super::result::ConnectionDetails {
        self.diagnostics.lock().unwrap().clone()
    }
    /// Reuse this source-bound client with a fresh cancellation lifetime.
    pub fn with_cancellation(&self, cancelled: Arc<AtomicBool>) -> Self {
        Self {
            cancelled,
            ..self.clone()
        }
    }
    fn request(
        &self,
        method: Method,
        route: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
        cleanup: bool,
    ) -> Result<Value> {
        self.request_budget(method, route, query, body, cleanup, Duration::from_secs(10))
    }
    fn request_budget(
        &self,
        method: Method,
        route: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
        cleanup: bool,
        budget: Duration,
    ) -> Result<Value> {
        ensure!(
            cleanup || !self.cancelled.load(Ordering::Relaxed),
            "Log request cancelled"
        );
        let mut url = self.endpoint.clone();
        url.set_path(&format!(
            "{}{route}",
            self.endpoint.path().trim_end_matches('/')
        ));
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query.iter().copied());
        }
        let mut authorization = zeroize::Zeroizing::new(b"ApiKey ".to_vec());
        authorization.extend_from_slice(&self.key.0);
        let mut auth = reqwest::header::HeaderValue::from_bytes(&authorization)?;
        auth.set_sensitive(true);
        drop(authorization);
        let mut request = self
            .client
            .request(method, url)
            .header(reqwest::header::AUTHORIZATION, auth)
            .header("kbn-xsrf", "true")
            .header("x-elastic-internal-origin", "Kibana")
            .header("elastic-api-version", "1");
        if let Some(body) = body {
            request = request.json(body);
        }
        #[cfg(test)]
        let virtual_time = self.test_clock.is_some();
        #[cfg(not(test))]
        let virtual_time = false;
        if !virtual_time {
            request = request.timeout(budget);
        }
        let diagnostic_route = if body.is_some_and(|b| b["id"].is_string()) {
            "async poll"
        } else if route == "/internal/search/es"
            && body.is_some_and(|b| b["params"]["aggs"].get("probe_0").is_some())
        {
            "fallback probe"
        } else {
            match route {
                "/internal/search/es" => "search",
                "/internal/data_views/_fields_for_wildcard" => "field catalog",
                "/api/status" => "version",
                _ => "data view",
            }
        };
        let execute = async {
            #[cfg(test)]
            if diagnostic_route == "async poll" {
                self.polls_started.fetch_add(1, Ordering::AcqRel);
            }
            let mut response = request.send().await.map_err(|error| {
                if error.is_timeout() {
                    anyhow::anyhow!(SearchTimeout)
                } else {
                    anyhow::anyhow!(Transient(
                        "Kibana connection failed; check VPN, HTTPS trust and the saved source"
                    ))
                }
            })?;
            let status = response.status().as_u16();
            self.diagnostics
                .lock()
                .unwrap()
                .route(diagnostic_route, &format!("HTTP {status}"));
            match status {
                407 => return Err(anyhow::anyhow!(Rejected(okilum_http::http_reason(407)))),
                401 | 403 => return Err(anyhow::anyhow!(AuthError { status })),
                300..=399 => bail!("Kibana redirected the request; credentials were not forwarded. Import the final HTTPS Discover URL"),
                400 => return Err(anyhow::anyhow!(Rejected("Kibana rejected the query or internal route (HTTP 400); this Kibana/Elasticsearch capability may be unsupported".into()))),
                500..=599 => return Err(anyhow::anyhow!(Transient("Kibana server is unavailable"))),
                200..=299 => {},
                _ => bail!("Kibana request failed (HTTP {status})"),
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|error| {
                if error.is_timeout() {
                    anyhow::anyhow!(SearchTimeout)
                } else {
                    anyhow::anyhow!(Transient("Kibana response was interrupted"))
                }
            })? {
                ensure!(
                    bytes.len() + chunk.len() <= RESPONSE_BYTES as usize,
                    "Kibana response exceeds 8 MiB; use a narrower time range"
                );
                bytes.extend_from_slice(&chunk);
            }
            Ok::<_, anyhow::Error>(bytes)
        };
        #[cfg(test)]
        let execute = async {
            if let Some(clock) = &self.test_clock {
                let began = clock.now();
                return tokio::select! {
                    result = execute => result,
                    _ = clock.deadline(began, budget) => Err(anyhow::anyhow!(SearchTimeout)),
                };
            }
            execute.await
        };
        let result = self.runtime.block_on(async {
            if cleanup {
                return execute.await;
            }
            tokio::select! {
                result = execute => result,
                _ = async {
                    while !self.cancelled.load(Ordering::Acquire) {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                } => bail!("Log request cancelled"),
            }
        });
        if let Err(error) = &result {
            let mut details = self.diagnostics.lock().unwrap();
            details.timeouts |= error.is::<SearchTimeout>();
            details.route(
                diagnostic_route,
                if error.is::<SearchTimeout>() {
                    "timeout"
                } else {
                    "request failed"
                },
            );
        }
        let bytes = result?;
        if bytes.is_empty() && cleanup {
            return Ok(Value::Null);
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
            self.diagnostics
                .lock()
                .unwrap()
                .route(diagnostic_route, "malformed JSON");
            anyhow::anyhow!("Kibana did not return valid JSON")
        })?;
        ensure!(
            value.get("error").is_none()
                && value
                    .get("statusCode")
                    .and_then(Value::as_u64)
                    .is_none_or(|s| s < 400),
            "Kibana/Elasticsearch returned an error; the previous applied query is retained"
        );
        Ok(value)
    }
}

impl Kibana {
    fn route_id(prefix: &str, id: &str) -> String {
        let encoded: String = url::form_urlencoded::byte_serialize(id.as_bytes()).collect();
        format!("{prefix}/{encoded}")
    }
    fn version(&self) -> Option<String> {
        let version = self
            .request(Method::GET, "/api/status", &[], None, false)
            .ok()
            .and_then(|v| {
                v["version"]["number"]
                    .as_str()
                    .filter(|s| {
                        s.len() <= 64
                            && s.chars()
                                .all(|c| c.is_ascii_alphanumeric() || ".-+".contains(c))
                    })
                    .map(str::to_owned)
            });
        self.diagnostics.lock().unwrap().version = version.clone();
        version
    }
    pub fn resolve_source(&self, source: &Source) -> Result<Source> {
        source.validate()?;
        ensure!(
            Url::parse(&source.endpoint)? == self.endpoint
                && self.source_id.is_none_or(|id| id == source.id),
            "Kibana client belongs to another source; create a client for the selected source"
        );
        if source.index_pattern.is_some() {
            return Ok(source.clone());
        }
        let route = Self::route_id("/api/data_views/data_view", &source.discover.data_view);
        let view = match self.request(Method::GET, &route, &[], None, false) {
            Ok(view) => view,
            Err(error)
                if error
                    .downcast_ref::<AuthError>()
                    .is_some_and(|auth| auth.status == 401) =>
            {
                return Err(error)
            }
            Err(_) => return Err(anyhow::anyhow!(IndexPatternRequired)),
        };
        let pattern = view["data_view"]["title"]
            .as_str()
            .ok_or(IndexPatternRequired)?;
        super::validate_pattern(pattern)?;
        let mut resolved = source.clone();
        resolved.index_pattern = Some(pattern.into());
        resolved.time_field = view["data_view"]["timeFieldName"]
            .as_str()
            .unwrap_or(&source.time_field)
            .into();
        if let Some(sort) = resolved.discover.sort.first_mut() {
            if sort.field == source.time_field {
                sort.field = resolved.time_field.clone();
            }
        }
        resolved.validate()?;
        Ok(resolved)
    }
    fn search(&self, params: &Value, deadline: Duration) -> Result<(Value, bool)> {
        let clock = self.now();
        let payload = json!({"params":params,"legacyHitsTotal":false});
        let mut response = self.request_budget(
            Method::POST,
            "/internal/search/es",
            &[],
            Some(&payload),
            false,
            deadline,
        )?;
        let mut id: Option<String> = None;
        loop {
            if let Some(next) = response["id"].as_str() {
                id = Some(next.into());
            }
            for flag in ["isRunning", "isPartial"] {
                ensure!(
                    response.get(flag).is_none_or(Value::is_boolean),
                    "Malformed Kibana search: invalid {flag}"
                );
            }
            if response["isRunning"] != true
                && !(response["isPartial"] == true && response["id"].is_string())
            {
                self.record_search_status(&response, params, false);
                return Ok((response, false));
            }
            let current = id
                .as_deref()
                .context("Malformed async search: missing id")?;
            if self.now().duration_since(clock) >= deadline {
                self.record_search_status(&response, params, true);
                return Ok((timeout_response(response), true));
            }
            // The verified protocol polls the same route with the current id
            // and unchanged flat params. Closing only cancels local polling.
            let poll_payload = json!({"id":current,"params":params,"legacyHitsTotal":false});
            for _ in 0..5 {
                if self.now().duration_since(clock) >= deadline {
                    break;
                }
                if self.cancelled.load(Ordering::Acquire) {
                    bail!("Log request cancelled");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            if self.now().duration_since(clock) >= deadline {
                continue;
            }
            match self.request_budget(
                Method::POST,
                "/internal/search/es",
                &[],
                Some(&poll_payload),
                false,
                deadline.saturating_sub(self.now().duration_since(clock)),
            ) {
                Ok(value) => response = value,
                Err(error) if error.is::<SearchTimeout>() => {
                    self.record_search_status(&response, params, true);
                    return Ok((timeout_response(response), true));
                }
                Err(error) => return Err(error),
            }
        }
    }
    fn record_search_status(&self, response: &Value, params: &Value, timeout: bool) {
        let mut details = self.diagnostics.lock().unwrap();
        details.timeouts |= timeout;
        if let Ok(status) = ResultStatus::parse(
            response,
            &params["track_total_hits"],
            params["index"].as_str().is_some_and(|p| p.contains(':')),
        ) {
            details.timeouts |= status.timed_out;
            details.status = Some(status);
        }
    }
    /// Kibana's field catalog distinguishes keyword multifields from analyzed text.
    /// This mapping-only path is optional; discovery falls back to search probes.
    fn mapped_fields(&self, source: &Source) -> Result<Vec<super::result::AggregatableField>> {
        let source = self.resolve_source(source)?;
        let pattern = source
            .index_pattern
            .as_deref()
            .context("Index pattern required")?;
        let response = self.request(
            Method::GET,
            "/internal/data_views/_fields_for_wildcard",
            &[("pattern", pattern)],
            None,
            false,
        )?;
        let fields = response["fields"]
            .as_array()
            .context("Malformed Kibana field catalog")?;
        ensure!(
            fields.len() <= 20_000,
            "Kibana field catalog exceeds 20,000 fields"
        );
        ensure!(!fields.is_empty(), "Empty Kibana field catalog");
        ensure!(
            fields.iter().all(|f| f["name"].is_string()
                && f["aggregatable"].is_boolean()
                && f["searchable"].is_boolean()),
            "Malformed Kibana field catalog entries"
        );
        let mut found = Vec::new();
        for field in fields {
            if field["aggregatable"] != true
                || field["searchable"] != true
                || field.pointer("/subType/nested").is_some()
            {
                continue;
            }
            let Some(name) = field["name"]
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 256)
            else {
                continue;
            };
            let kinds = field["esTypes"]
                .as_array()
                .context("Kibana field catalog lacks Elasticsearch types")?;
            let field_type = if kinds.iter().all(|v| {
                matches!(
                    v.as_str(),
                    Some("keyword" | "constant_keyword" | "wildcard")
                )
            }) && !kinds.is_empty()
            {
                FieldType::String
            } else if kinds.iter().all(|v| {
                matches!(
                    v.as_str(),
                    Some(
                        "byte"
                            | "short"
                            | "integer"
                            | "long"
                            | "unsigned_long"
                            | "half_float"
                            | "float"
                            | "double"
                            | "scaled_float"
                    )
                )
            }) && !kinds.is_empty()
            {
                FieldType::Number
            } else if kinds.as_slice() == [Value::String("boolean".into())] {
                FieldType::Boolean
            } else {
                continue;
            };
            let source_path = field
                .pointer("/subType/multi/parent")
                .and_then(Value::as_str)
                .unwrap_or(name)
                .to_owned();
            found.push(super::result::AggregatableField {
                name: name.into(),
                field_type,
                source_path,
            });
        }
        found.sort_by(|a, b| a.name.cmp(&b.name));
        ensure!(
            found
                .windows(2)
                .all(|pair| pair[0].name != pair[1].name || pair[0] == pair[1]),
            "Conflicting Kibana field catalog entries; no mapping was selected"
        );
        found.dedup_by(|a, b| a.name == b.name);
        ensure!(
            !found.is_empty(),
            "No scalar fields in Kibana field catalog"
        );
        Ok(found)
    }
    /// Top-N server aggregation over the same source, absolute time and server
    /// filters as history. Missing is a separate aggregation, never a sentinel key.
    pub fn field_values(
        &self,
        source: &Source,
        request: &FieldValuesRequest,
    ) -> Result<FieldValues> {
        ensure!(
            !request.field.is_empty()
                && request.field.len() <= 256
                && (1..=1000).contains(&request.limit),
            "Invalid field-values field or limit (1–1000)"
        );
        ensure!(
            matches!(
                request.field_type,
                FieldType::String | FieldType::Number | FieldType::Boolean
            ),
            "Field values require a scalar field type"
        );
        let source = self.resolve_source(source)?;
        let range = source
            .discover
            .time
            .resolve(time::OffsetDateTime::now_utc())?;
        let mut params = source
            .discover
            .search_body_for(&range, None, 0, &source.time_field)?;
        params.as_object_mut().unwrap().remove("sort");
        params["index"] = json!(source.index_pattern.as_ref().unwrap());
        params["aggs"] = json!({"values":{"terms":{"field":request.field,"size":request.limit}},"missing_values":{"missing":{"field":request.field}}});
        let (response, timeout) = self.search(&params, Duration::from_secs(10))?;
        let mut status = ResultStatus::parse(
            &response,
            &params["track_total_hits"],
            source.index_pattern.as_ref().unwrap().contains(':'),
        )?;
        status.timed_out |= timeout;
        status.partial |= timeout;
        let aggs = &response["rawResponse"]["aggregations"];
        if aggs.is_null() && status.timed_out {
            return Err(anyhow::anyhow!(SearchTimeout));
        }
        let mut buckets = Vec::new();
        for bucket in aggs["values"]["buckets"]
            .as_array()
            .context("Malformed field-values buckets")?
        {
            let value = match (&bucket["key"], request.field_type) {
                (Value::Bool(value), FieldType::Boolean) => FieldValue::Boolean(*value),
                (Value::Number(_), FieldType::Boolean) => match bucket["key_as_string"].as_str() {
                    Some("true") => FieldValue::Boolean(true),
                    Some("false") => FieldValue::Boolean(false),
                    _ => bail!("Malformed boolean aggregation key"),
                },
                (Value::Number(value), FieldType::Number) => FieldValue::Number(value.clone()),
                (Value::String(value), FieldType::String) => FieldValue::String(value.clone()),
                _ => bail!("Unexpected field-values key type"),
            };
            buckets.push(FieldBucket {
                value,
                count: bucket["doc_count"]
                    .as_u64()
                    .context("Missing bucket count")?,
            });
        }
        Ok(FieldValues {
            buckets,
            missing_count: aggs["missing_values"]["doc_count"]
                .as_u64()
                .context("Missing missing-value count")?,
            sum_other_doc_count: aggs["values"]["sum_other_doc_count"]
                .as_u64()
                .context("Missing other-value count")?,
            doc_count_error_upper_bound: aggs["values"]["doc_count_error_upper_bound"].as_u64(),
            status,
        })
    }
}

fn timeout_response(mut response: Value) -> Value {
    if response.get("rawResponse").is_none() {
        response["rawResponse"] = json!({});
    }
    if response["rawResponse"].is_object() && response["rawResponse"].get("hits").is_none() {
        response["rawResponse"]["hits"] = json!({"hits":[]});
    }
    response
}

#[derive(Clone, Debug)]
pub struct Page {
    pub events: Vec<Value>,
    pub complete: bool,
    pub limited: bool,
    pub range: TimeRange,
    pub cleanup_warning: Option<String>,
    pub status: ResultStatus,
}

/// Bounded non-snapshot paging. Time is primary; original cursor values are
/// forwarded verbatim. Boundary replay includes equal-time documents whose _doc
/// values collide across shards. The whole window is reconciled again in live.
pub struct History {
    kibana: Kibana,
    pub state: Discover,
    pub range: TimeRange,
    after: Option<Value>,
    seen: HashSet<(String, String)>,
    bytes: usize,
    finished: bool,
    pub pattern: String,
    time_field: String,
    preference: String,
    status: ResultStatus,
    boundary: Option<(String, usize)>,
    main_exhausted: bool,
    search_timeout: Duration,
}
impl History {
    pub fn set_cancellation(&mut self, cancelled: Arc<AtomicBool>) {
        self.kibana.cancelled = cancelled;
    }
    pub fn can_page(&self) -> bool {
        !self.finished
    }
    pub fn open(kibana: Kibana, state: Discover) -> Result<Self> {
        let source = Source {
            id: kibana.source_id.unwrap_or_else(uuid::Uuid::new_v4),
            name: "History".into(),
            endpoint: kibana.endpoint.to_string(),
            discover: state,
            index_pattern: None,
            time_field: "@timestamp".into(),
            scope_fields: Vec::new(),
        };
        Self::for_source(kibana, &source)
    }
    pub fn for_source(kibana: Kibana, source: &Source) -> Result<Self> {
        let source = kibana.resolve_source(source)?;
        ensure!(
            source.discover.sort[0].field == source.time_field,
            "History requires the time field as the primary sort"
        );
        let range = source
            .discover
            .time
            .resolve(time::OffsetDateTime::now_utc())?;
        let version = kibana.version();
        Ok(Self {
            kibana,
            state: source.discover,
            range,
            after: None,
            seen: HashSet::new(),
            bytes: 0,
            finished: false,
            pattern: source.index_pattern.unwrap(),
            time_field: source.time_field,
            preference: uuid::Uuid::new_v4().to_string(),
            status: ResultStatus {
                version,
                ..Default::default()
            },
            boundary: None,
            main_exhausted: false,
            search_timeout: Duration::from_secs(10),
        })
    }
    pub fn page(&mut self) -> Result<Page> {
        ensure!(
            !self.finished,
            "History has ended; Apply to start a new query"
        );
        let result = self.read_page();
        if result.is_err() {
            self.finished = true;
        }
        result
    }
    fn read_page(&mut self) -> Result<Page> {
        // A replay can contain only duplicates. Do not shrink HTTP pages to
        // the remaining unique-row budget and re-read that prefix one row at a time.
        let size = PAGE_ROWS;
        ensure!(
            self.seen.len() < MAX_ROWS && self.bytes < MAX_EVENT_BYTES,
            "Loaded history reached the limit"
        );
        let replay = self.boundary.clone();
        let mut params = self.state.search_body_for(
            &self.range,
            if replay.is_none() {
                self.after.as_ref()
            } else {
                None
            },
            size,
            &self.time_field,
        )?;
        params["index"] = json!(self.pattern);
        params["preference"] = json!(self.preference);
        if let Some((stamp, offset)) = &replay {
            // Preserve the selected half-open interval AND include both sides of
            // the exact boundary timestamp. Offset is bounded by the row cap.
            params["query"]["bool"]["filter"].as_array_mut().unwrap().push(json!({"range":{&self.time_field:{"gte":stamp,"lte":stamp,"format":"strict_date_optional_time_nanos"}}}));
            params["from"] = json!(offset);
            params["size"] = json!(size.min(MAX_ROWS - offset));
        }
        let (response, timeout) = self.kibana.search(&params, self.search_timeout)?;
        let mut quality = ResultStatus::parse(
            &response,
            &params["track_total_hits"],
            self.pattern.contains(':'),
        )?;
        quality.timed_out |= timeout;
        quality.partial |= timeout;
        // Boundary aggregations describe a subset; retain the main-query total.
        if replay.is_some() {
            quality.total = None;
        }
        self.status.accumulate(quality);
        let hits = response["rawResponse"]["hits"]["hits"]
            .as_array()
            .context("Malformed Kibana search: hits are missing")?;
        let requested = params["size"].as_u64().unwrap() as usize;
        ensure!(
            hits.len() <= requested,
            "Server exceeded the requested page size"
        );
        let mut events = Vec::new();
        for hit in hits {
            let key = (
                hit["_index"]
                    .as_str()
                    .context("Missing event index")?
                    .to_owned(),
                hit["_id"].as_str().context("Missing event id")?.to_owned(),
            );
            ensure!(
                hit["sort"]
                    .as_array()
                    .is_some_and(|a| a.len() == self.state.sort.len() + 1
                        && a.iter().all(|v| v.is_string() || v.is_number())),
                "Missing or unsupported history cursor"
            );
            if self.seen.contains(&key) {
                continue;
            }
            let bytes = serde_json::to_vec(hit)?.len();
            if self.seen.len() >= MAX_ROWS || self.bytes + bytes > MAX_EVENT_BYTES {
                self.status.truncated = true;
                break;
            }
            self.bytes += bytes;
            self.seen.insert(key);
            events.push(hit.clone());
        }
        if self.seen.len() >= MAX_ROWS || self.bytes >= MAX_EVENT_BYTES {
            self.status.truncated = true;
        }
        if let Some((stamp, offset)) = replay {
            if let Some(last) = hits.last() {
                self.after = Some(last["sort"].clone());
            }
            if hits.len() < requested {
                self.boundary = None;
                self.finished = self.main_exhausted;
            } else if offset + hits.len() >= MAX_ROWS {
                self.status.truncated = true;
            } else {
                self.boundary = Some((stamp, offset + hits.len()));
            }
        } else {
            self.main_exhausted = hits.len() < size;
            if let Some(last) = hits.last() {
                let cursor = last["sort"].clone();
                ensure!(
                    self.after.as_ref() != Some(&cursor),
                    "History cursor did not advance"
                );
                let stamp = timestamp(&cursor[0])?;
                self.after = Some(cursor);
                self.boundary = Some((stamp, 0));
            } else {
                self.finished = true;
            }
        }
        self.finished |= self.status.incomplete();
        Ok(Page {
            events,
            complete: self.finished && !self.status.incomplete(),
            limited: self.status.truncated,
            range: self.range.clone(),
            cleanup_warning: None,
            status: self.status.clone(),
        })
    }
    pub fn close(&mut self) -> Result<()> {
        self.finished = true;
        Ok(())
    }
}
fn timestamp(value: &Value) -> Result<String> {
    if let Some(stamp) = value.as_str() {
        return Ok(stamp.into());
    }
    // date_nanos numbers exceed 2^53. Parse the JSON integer directly, never f64.
    let nanos: i128 = value
        .to_string()
        .parse()
        .context("Unsupported numeric time cursor")?;
    Ok(time::OffsetDateTime::from_unix_timestamp_nanos(nanos)?
        .format(&time::format_description::well_known::Rfc3339)?)
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;
