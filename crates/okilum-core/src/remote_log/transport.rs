//! Version-specific Kibana transport. No redirects, secret diagnostics, or disk
//! event cache. A history session must be owned by a background worker.
use super::{
    discover::{Discover, TimeRange},
    Source, MAX_EVENT_BYTES, MAX_ROWS, PAGE_ROWS,
};
use anyhow::{bail, ensure, Context, Result};
use reqwest::{Client, Method};
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use url::Url;

const RESPONSE_BYTES: u64 = 8 * 1024 * 1024;

pub struct ApiKey(Vec<u8>);
impl ApiKey {
    pub fn new(bytes: Vec<u8>) -> Result<Self> {
        ensure!(
            !bytes.is_empty() && bytes.len() <= 8192 && bytes.iter().all(|b| b.is_ascii_graphic()),
            "Enter an encoded Kibana API key without whitespace"
        );
        Ok(Self(bytes))
    }
}
impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey([redacted])")
    }
}
impl Drop for ApiKey {
    fn drop(&mut self) {
        self.0.fill(0);
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
pub(super) struct Incomplete(pub String);
impl std::fmt::Display for Incomplete {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Incomplete {}

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
    key: Arc<ApiKey>,
    runtime: Arc<tokio::runtime::Runtime>,
    cancelled: Arc<AtomicBool>,
}
impl Kibana {
    pub fn new(source: &Source, key: ApiKey, cancelled: Arc<AtomicBool>) -> Result<Self> {
        source.validate()?;
        Self::with_endpoint(Url::parse(&source.endpoint)?, key, cancelled)
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
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(10))
            .build()?;
        Ok(Self {
            client,
            endpoint,
            key: Arc::new(key),
            runtime,
            cancelled,
        })
    }
    pub(super) fn with_cancellation(&self, cancelled: Arc<AtomicBool>) -> Self {
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
        proxy: bool,
        cleanup: bool,
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
        let mut authorization = b"ApiKey ".to_vec();
        authorization.extend_from_slice(&self.key.0);
        let mut auth = reqwest::header::HeaderValue::from_bytes(&authorization)?;
        auth.set_sensitive(true);
        authorization.fill(0);
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
        request = request.timeout(
            if cleanup
                && !query
                    .iter()
                    .any(|(key, value)| *key == "method" && *value == "POST")
            {
                Duration::from_secs(2)
            } else {
                Duration::from_secs(10)
            },
        );
        let execute = async {
            let mut response = request.send().await.map_err(|error| {
                anyhow::anyhow!(Transient(if error.is_timeout() {
                    "Kibana request timed out; check VPN and server availability"
                } else {
                    "Kibana connection failed; check VPN, HTTPS trust and the saved source"
                }))
            })?;
            let status = response.status().as_u16();
            let backend = response
                .headers()
                .get("x-console-proxy-status-code")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u16>().ok());
            match status {
                401 | 403 => return Err(anyhow::anyhow!(Rejected(format!("Kibana authentication or permission denied (HTTP {status}); update this source's OS-stored key")))),
                300..=399 => bail!("Kibana redirected the request; credentials were not forwarded. Import the final HTTPS Discover URL"),
                400 => return Err(anyhow::anyhow!(Rejected("Kibana rejected the query or internal route (HTTP 400); this Kibana/Elasticsearch capability may be unsupported".into()))),
                500..=599 => return Err(anyhow::anyhow!(Transient("Kibana server is unavailable"))),
                200..=299 => {},
                _ => bail!("Kibana request failed (HTTP {status})"),
            }
            if proxy {
                ensure!(backend.is_some_and(|s| (200..300).contains(&s)), "Elasticsearch console-proxy request failed or its backend status is unknown ({backend:?})");
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| anyhow::anyhow!(Transient("Kibana response was interrupted")))?
            {
                ensure!(
                    bytes.len() + chunk.len() <= RESPONSE_BYTES as usize,
                    "Kibana response exceeds 8 MiB; use a narrower time range"
                );
                bytes.extend_from_slice(&chunk);
            }
            Ok::<_, anyhow::Error>(bytes)
        };
        let bytes = self.runtime.block_on(async {
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
        })?;
        let value: Value =
            serde_json::from_slice(&bytes).context("Kibana did not return valid JSON")?;
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
    fn proxy(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        cleanup: bool,
    ) -> Result<Value> {
        self.request(
            Method::POST,
            "/api/console/proxy",
            &[("path", path), ("method", method)],
            body,
            true,
            cleanup,
        )
    }
    fn route_id(prefix: &str, id: &str) -> String {
        let encoded: String = url::form_urlencoded::byte_serialize(id.as_bytes()).collect();
        format!("{prefix}/{encoded}")
    }
}

#[derive(Clone, Debug)]
pub struct Page {
    /// Original server hits: all source fields, doc values and document identity.
    pub events: Vec<Value>,
    pub complete: bool,
    pub limited: bool,
    pub range: TimeRange,
    pub cleanup_warning: Option<String>,
}

pub struct History {
    kibana: Kibana,
    pub state: Discover,
    pub range: TimeRange,
    pit: Option<String>,
    async_ids: HashSet<String>,
    after: Option<Value>,
    seen: HashSet<(String, String)>,
    bytes: usize,
    finished: bool,
    unacknowledged_search: bool,
    pub pattern: String,
}
// A failed acquisition still owns a received handle when cleanup is unavailable.
// The session extracts this owner before returning the underlying visible error.
pub(super) struct AcquireFailure {
    pub error: anyhow::Error,
    pub history: Box<History>,
}
impl std::fmt::Debug for AcquireFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("AcquireFailure").field(&self.error).finish()
    }
}
impl std::fmt::Display for AcquireFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.error)
    }
}
impl std::error::Error for AcquireFailure {}

impl History {
    pub fn set_cancellation(&mut self, cancelled: Arc<AtomicBool>) {
        self.kibana.cancelled = cancelled;
    }
    pub(super) fn uncertain(&self) -> bool {
        self.unacknowledged_search
    }
    pub(super) fn has_resources(&self) -> bool {
        self.unacknowledged_search || self.pit.is_some() || !self.async_ids.is_empty()
    }
    pub fn can_page(&self) -> bool {
        !self.finished && !self.unacknowledged_search && self.pit.is_some()
    }
    pub fn open(kibana: Kibana, state: Discover) -> Result<Self> {
        state.validate()?;
        let range = state.time.resolve(time::OffsetDateTime::now_utc())?;
        // Guard the internal transport by the version actually researched. A
        // different version is a visible limitation, not implicit compatibility.
        let status = kibana.request(Method::GET, "/api/status", &[], None, false, false)?;
        ensure!(
            status["version"]["number"] == "9.5.4",
            "This source is not Kibana 9.5.4; its internal search contract has not been verified"
        );
        let route = Kibana::route_id("/api/data_views/data_view", &state.data_view);
        let view = kibana.request(Method::GET, &route, &[], None, false, false)?;
        let pattern = view["data_view"]["title"]
            .as_str()
            .context("Data view did not resolve to an index pattern")?
            .to_owned();
        ensure!(
            !pattern.is_empty()
                && pattern.len() <= 1024
                && !pattern.contains(['?', '#', '&', '/'])
                && !pattern.chars().any(char::is_whitespace),
            "Unsupported data-view index pattern"
        );
        ensure!(
            view["data_view"]["timeFieldName"] == "@timestamp",
            "Only data views using @timestamp are currently supported"
        );
        let path = format!("{pattern}/_pit?keep_alive=1m&allow_partial_search_results=false");
        ensure!(
            !kibana.cancelled.load(Ordering::Acquire),
            "Log request cancelled"
        );
        // Shield handle acquisition from local cancellation, then close a stale
        // acquired handle. An unreceived handle cannot be retried as an outage.
        let opened = kibana.proxy("POST", &path, None, true).map_err(|error| anyhow::anyhow!("Cannot open history snapshot: {error}; server cleanup is unconfirmed if an ID was not received; automatic retry stopped"))?;
        let pit = opened["id"]
            .as_str()
            .context("PIT ID missing; server cleanup is unconfirmed")?
            .to_owned();
        let mut history = Self {
            kibana,
            state,
            range,
            pit: Some(pit),
            async_ids: HashSet::new(),
            after: None,
            seen: HashSet::new(),
            bytes: 0,
            finished: false,
            unacknowledged_search: false,
            pattern,
        };
        // A successfully acquired handle is already guarded before inspecting quality.
        if let Err(error) = quality(&opened, history.pattern.contains(':')) {
            let cleanup = history.close();
            let error = anyhow::anyhow!(Incomplete(format!(
                "{error}; PIT cleanup {}",
                if cleanup.is_ok() {
                    "confirmed"
                } else {
                    "unconfirmed"
                }
            )));
            if history.has_resources() {
                return Err(anyhow::anyhow!(AcquireFailure {
                    error,
                    history: Box::new(history)
                }));
            }
            return Err(error);
        }
        Ok(history)
    }
    pub fn page(&mut self) -> Result<Page> {
        ensure!(
            !self.finished,
            "This history snapshot has ended; Apply to start a new query"
        );
        match self.read_page() {
            Ok(page) => Ok(page),
            Err(error) => {
                let cleanup = self.close();
                if self.unacknowledged_search {
                    return Err(anyhow::anyhow!(Incomplete(format!("{error:#}; latest PIT/async handle may be unreceived. Cleanup unconfirmed; automatic continuation stopped"))));
                }
                match cleanup {
                    Ok(()) => Err(error),
                    Err(_) => Err(error.context("Resource cleanup could not be confirmed")),
                }
            }
        }
    }
    fn read_page(&mut self) -> Result<Page> {
        let pit = self
            .pit
            .clone()
            .context("History snapshot has been closed; Apply to restart")?;
        let size = PAGE_ROWS.min(MAX_ROWS.saturating_sub(self.seen.len()));
        ensure!(
            size > 0 && self.bytes < MAX_EVENT_BYTES,
            "Loaded history reached the 10,000-event / 16 MiB limit"
        );
        let body = self
            .state
            .search_body(&self.range, self.after.as_ref(), size)?;
        let payload = json!({"params":{"pit":{"id":pit,"keep_alive":"1m"},"body":body,"track_total_hits":false,"timeout":"5s","allow_partial_search_results":false},"legacyHitsTotal":false});
        ensure!(
            !self.kibana.cancelled.load(Ordering::Acquire),
            "Log request cancelled"
        );
        self.unacknowledged_search = true;
        let mut response = self
            .kibana
            .request(
                Method::POST,
                "/internal/search/es",
                &[],
                Some(&payload),
                false,
                false,
            )
            .map_err(|error| {
                if error.is::<Rejected>() {
                    self.unacknowledged_search = false;
                }
                error
            })?;
        for poll in 0..=2 {
            if let Some(id) = response.get("id").and_then(Value::as_str) {
                self.async_ids.insert(id.into());
            }
            let raw = response.get("rawResponse").unwrap_or(&response);
            if let Some(id) = raw.get("pit_id").and_then(Value::as_str) {
                self.pit = Some(id.to_owned());
                self.unacknowledged_search = false;
            }
            ensure!(
                response["isRunning"].is_boolean() && response["isPartial"].is_boolean(),
                "Search completion metadata is unknown"
            );
            if response["isRunning"] == false {
                self.async_ids.clear();
                break;
            }
            ensure!(
                poll < 2,
                "Search is still running after the bounded async wait; narrow the range"
            );
            let id = response["id"]
                .as_str()
                .context("Async search ID is missing")?;
            let route = Kibana::route_id("/internal/search/es", id);
            self.unacknowledged_search = true;
            response =
                self.kibana
                    .request(Method::POST, &route, &[], Some(&payload), false, false)?;
        }
        if self.unacknowledged_search {
            return Err(anyhow::anyhow!(Incomplete(
                "Search did not acknowledge its current PIT handle".into()
            )));
        }
        if response["isPartial"] != false {
            return Err(anyhow::anyhow!(Incomplete(
                "Kibana returned an incomplete result; previous results are retained".into()
            )));
        }
        let raw = response
            .get("rawResponse")
            .context("Search raw response is missing")?;
        ensure!(
            raw["timed_out"] == false,
            "Search timed out or its timeout status is unknown"
        );
        quality(raw, self.pattern.contains(':'))
            .map_err(|error| anyhow::anyhow!(Incomplete(error.to_string())))?;
        let hits = raw["hits"]["hits"]
            .as_array()
            .context("Search hits are missing")?;
        ensure!(
            hits.len() <= size,
            "Server exceeded the requested page size"
        );
        let next = hits.last().map(|hit| hit["sort"].clone());
        if let Some(next) = &next {
            ensure!(
                next.as_array()
                    .is_some_and(|a| a.len() == self.state.sort.len() + 1
                        && a.iter().all(|v| v.is_string() || v.is_number())),
                "History cursor is missing or unsupported"
            );
            ensure!(
                self.after.as_ref() != Some(next),
                "History cursor did not advance"
            );
        }
        let mut events = Vec::new();
        let mut limited = false;
        for hit in hits {
            let index = hit["_index"]
                .as_str()
                .context("Event index identity is missing")?;
            let id = hit["_id"]
                .as_str()
                .context("Event document identity is missing")?;
            let key = (index.to_owned(), id.to_owned());
            if self.seen.contains(&key) {
                continue;
            }
            let bytes = serde_json::to_vec(hit)?.len();
            if self.bytes + bytes > MAX_EVENT_BYTES || self.seen.len() == MAX_ROWS {
                limited = true;
                break;
            }
            self.bytes += bytes;
            self.seen.insert(key);
            events.push(hit.clone());
        }
        self.after = next;
        limited |= self.bytes >= MAX_EVENT_BYTES || self.seen.len() >= MAX_ROWS;
        let complete = hits.len() < size && !limited;
        self.finished = complete || limited;
        let cleanup_warning = if self.finished {
            self.close()
                .err()
                .map(|_| "History loaded, but PIT/async cleanup could not be confirmed".into())
        } else {
            None
        };
        Ok(Page {
            events,
            complete,
            limited,
            range: self.range.clone(),
            cleanup_warning,
        })
    }
    pub fn close(&mut self) -> Result<()> {
        self.finished = true;
        let mut failed = self.unacknowledged_search;
        for id in self.async_ids.clone() {
            let route = Kibana::route_id("/internal/search/es", &id);
            if self
                .kibana
                .request(Method::DELETE, &route, &[], None, false, true)
                .is_ok()
            {
                self.async_ids.remove(&id);
            } else {
                failed = true;
            }
        }
        if let Some(id) = self.pit.clone() {
            match self
                .kibana
                .proxy("DELETE", "_pit", Some(&json!({"id":id})), true)
            {
                Ok(value)
                    if value["succeeded"] == true && value["num_freed"].as_u64().is_some() =>
                {
                    self.pit = None
                }
                _ => failed = true,
            }
        }
        ensure!(!failed, "PIT or async cleanup could not be confirmed");
        Ok(())
    }
}
impl Drop for History {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn quality(raw: &Value, ccs: bool) -> Result<()> {
    let shards = raw
        .get("_shards")
        .context("Shard completion metadata is unknown")?;
    let total = shards["total"].as_u64().context("Shard total is unknown")?;
    ensure!(
        shards["failed"] == 0 && shards["successful"].as_u64() == Some(total),
        "Some search shards failed or did not complete"
    );
    if let Some(clusters) = raw.get("_clusters") {
        let total = clusters["total"]
            .as_u64()
            .context("Cluster total is unknown")?;
        ensure!(
            clusters["successful"].as_u64() == Some(total),
            "Cross-cluster search is incomplete"
        );
        for key in ["skipped", "partial", "failed", "running"] {
            ensure!(
                clusters[key].as_u64() == Some(0),
                "Cross-cluster {key} result is nonzero or unknown"
            );
        }
    } else {
        ensure!(!ccs, "Cross-cluster completion metadata is unknown");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Read, Write},
        net::TcpListener,
        sync::Mutex,
        thread,
    };
    struct Reply {
        status: u16,
        body: Value,
        backend: Option<u16>,
    }
    type RecordedCalls = Arc<Mutex<Vec<(String, Value)>>>;
    fn ok(body: Value) -> Reply {
        Reply {
            status: 200,
            body,
            backend: Some(200),
        }
    }
    fn server(replies: Vec<Reply>) -> (Url, RecordedCalls, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint =
            Url::parse(&format!("http://{}/s/test", listener.local_addr().unwrap())).unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let thread = thread::spawn(move || {
            for reply in replies {
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                std::time::Instant::now() < deadline,
                                "expected HTTP request never arrived"
                            );
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("mock listener failed: {error}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                let mut size = 0;
                let mut auth = false;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    let lower = line.to_ascii_lowercase();
                    if let Some(value) = lower.strip_prefix("content-length:") {
                        size = value.trim().parse().unwrap();
                    }
                    if lower.starts_with("authorization:") {
                        auth = line.contains("ApiKey test-secret");
                    }
                }
                assert!(
                    auth,
                    "positive control: actual HTTP request contains the key header"
                );
                let mut body = vec![0; size];
                reader.read_exact(&mut body).unwrap();
                let body = if body.is_empty() {
                    Value::Null
                } else {
                    serde_json::from_slice(&body).unwrap()
                };
                recorded.lock().unwrap().push((first.trim().into(), body));
                let bytes = serde_json::to_vec(&reply.body).unwrap();
                let backend = reply
                    .backend
                    .map(|s| format!("x-console-proxy-status-code: {s}\r\n"))
                    .unwrap_or_default();
                write!(stream,"HTTP/1.1 {} OK\r\nConnection: close\r\nContent-Type: application/json\r\n{backend}Content-Length: {}\r\n\r\n",reply.status,bytes.len()).unwrap();
                stream.write_all(&bytes).unwrap();
            }
        });
        (endpoint, calls, thread)
    }
    fn controls() -> Vec<Reply> {
        let mut pit = super::super::tests::quality();
        pit["id"] = json!("pit-original");
        vec![
            ok(json!({"version":{"number":"9.5.4"}})),
            ok(json!({"data_view":{"title":"*:log-2","timeFieldName":"@timestamp"}})),
            ok(pit),
        ]
    }
    fn search(hits: Vec<Value>, pit: &str) -> Reply {
        let mut raw = super::super::tests::quality();
        raw["pit_id"] = json!(pit);
        raw["timed_out"] = json!(false);
        raw["hits"] = json!({"hits":hits});
        ok(json!({"isRunning":false,"isPartial":false,"rawResponse":raw}))
    }
    fn hit(id: usize) -> Value {
        json!({"_index":"log-2","_id":id.to_string(),"_source":{"@timestamp":"2026-10-20T12:00:00.123456789Z","description":"שלום","all":{"nested":[1,2]}},"sort":["2026-10-20T12:00:00.123456789Z",9223372036854775807u64-id as u64]})
    }
    fn client(endpoint: Url) -> Kibana {
        Kibana::with_endpoint(
            endpoint,
            ApiKey::new(b"test-secret".to_vec()).unwrap(),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap()
    }
    #[test]
    fn actual_http_paging_preserves_cursor_fields_identity_and_latest_scalar_cleanup() {
        let mut replies = controls();
        replies.push(search((0..PAGE_ROWS).map(hit).collect(), "pit-page-1"));
        replies.push(search(
            vec![hit(PAGE_ROWS - 1), hit(PAGE_ROWS)],
            "pit-latest",
        ));
        replies.push(ok(json!({"succeeded":true,"num_freed":273})));
        let (endpoint, calls, thread) = server(replies);
        let mut history = History::open(client(endpoint), super::super::tests::state()).unwrap();
        let first = history.page().unwrap();
        assert!(!first.complete);
        assert_eq!(first.events.len(), PAGE_ROWS);
        let second = history.page().unwrap();
        assert!(second.complete);
        assert_eq!(second.events.len(), 1);
        assert_eq!(second.events[0]["_source"]["all"]["nested"], json!([1, 2]));
        assert!(second.cleanup_warning.is_none());
        drop(history);
        thread.join().unwrap();
        let calls = calls.lock().unwrap();
        assert!(calls[3].0.starts_with("POST /s/test/internal/search/es "));
        assert!(calls[3].1["params"].get("index").is_none());
        assert_eq!(
            calls[4].1["params"]["body"]["search_after"],
            first.events.last().unwrap()["sort"]
        );
        assert_eq!(calls.last().unwrap().1, json!({"id":"pit-latest"}));
    }
    #[test]
    fn incomplete_search_still_closes_rotated_pit_and_does_not_accept_rows() {
        let mut replies = controls();
        let mut partial = search(vec![hit(0)], "rotated-before-error");
        partial.body["rawResponse"]["_clusters"]["skipped"] = json!(1);
        replies.push(partial);
        replies.push(ok(json!({"succeeded":true,"num_freed":273})));
        let (endpoint, calls, thread) = server(replies);
        let mut history = History::open(client(endpoint), super::super::tests::state()).unwrap();
        assert!(history.page().unwrap_err().to_string().contains("skipped"));
        drop(history);
        thread.join().unwrap();
        assert_eq!(
            calls.lock().unwrap().last().unwrap().1,
            json!({"id":"rotated-before-error"})
        );
    }
    #[test]
    fn proxy_outer_success_cannot_hide_backend_failure_and_key_never_enters_error() {
        let mut replies = controls();
        replies[2].backend = Some(400);
        replies[2].body = json!({"error":"echo test-secret"});
        let (endpoint, _, thread) = server(replies);
        let error = History::open(client(endpoint), super::super::tests::state())
            .err()
            .unwrap();
        assert!(!format!("{error:#}").contains("test-secret"));
        assert!(format!("{error:#}").contains("backend status"));
        thread.join().unwrap();
    }
    #[test]
    fn cleanup_failure_is_visible_and_cancellation_still_attempts_cleanup() {
        let mut replies = controls();
        replies.push(Reply {
            status: 200,
            backend: Some(200),
            body: json!({"succeeded":false,"num_freed":0}),
        });
        let (endpoint, calls, thread) = server(replies);
        let client = client(endpoint);
        let token = client.cancelled.clone();
        let mut history = History::open(client, super::super::tests::state()).unwrap();
        token.store(true, Ordering::Relaxed);
        let error = format!("{:#}", history.page().unwrap_err());
        assert!(error.contains("cancelled") && error.contains("cleanup"));
        drop(history);
        thread.join().unwrap();
        assert_eq!(
            calls.lock().unwrap().last().unwrap().1,
            json!({"id":"pit-original"})
        );
    }
    #[test]
    fn errors_and_redirects_are_distinct_without_forwarding_credentials() {
        for status in [302, 400, 401] {
            let (endpoint, calls, thread) = server(vec![Reply {
                status,
                backend: None,
                body: json!({"echo":"test-secret"}),
            }]);
            let error = History::open(client(endpoint), super::super::tests::state())
                .err()
                .unwrap()
                .to_string();
            assert!(!error.contains("test-secret"));
            assert!(match status {
                302 => error.contains("not forwarded"),
                400 => error.contains("unsupported"),
                _ => error.contains("authentication"),
            });
            thread.join().unwrap();
            assert_eq!(calls.lock().unwrap().len(), 1);
        }
    }
}
