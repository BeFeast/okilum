//! Synthetic loopback Kibana for protocol tests and debug-build GUI QA.
//! This module is excluded from release builds. It never reads external files.
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

#[derive(Clone, Debug, Default)]
pub struct Scenario {
    pub async_polls: usize,
    pub partial_with_id: bool,
    pub never_complete: bool,
    pub running_without_hits: bool,
    pub partial: bool,
    pub timed_out: bool,
    pub legacy_total: bool,
    pub lower_bound: bool,
    pub status_denied: bool,
    pub data_view_denied: bool,
    pub reject: bool,
    pub malformed: bool,
    pub unavailable: bool,
    pub forbidden: bool,
    pub failure_status: Option<u16>,
    pub omit_completion_flags: bool,
    pub delay: Duration,
}
#[derive(Clone, Debug)]
pub struct Call {
    pub method: String,
    pub path: String,
    pub body: Value,
}
#[derive(Default)]
pub struct State {
    pub records: Vec<Value>,
    pub scenario: Scenario,
    pub calls: Vec<Call>,
    pub polls: usize,
    pub searches: usize,
    pub rejected_headers: usize,
    next_id: usize,
    pending: BTreeMap<String, (Value, usize, Value)>,
}
pub struct FakeKibana {
    pub endpoint: String,
    pub state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}
impl FakeKibana {
    pub fn start(address: &str, count: usize, scenario: Scenario) -> std::io::Result<Self> {
        let listener = TcpListener::bind(address)?;
        listener.set_nonblocking(true)?;
        let endpoint = format!("http://{}", listener.local_addr()?);
        let state = Arc::new(Mutex::new(State {
            records: (0..count).map(record).collect(),
            scenario,
            ..Default::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let (shared, done) = (state.clone(), stop.clone());
        let thread = thread::spawn(move || {
            let mut children = Vec::new();
            while !done.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let state = shared.clone();
                        children.push(thread::spawn(move || {
                            let _ = respond(stream, state);
                        }));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(_) => break,
                }
                let mut i = 0;
                while i < children.len() {
                    if children[i].is_finished() {
                        let _ = children.swap_remove(i).join();
                    } else {
                        i += 1;
                    }
                }
            }
            for child in children {
                let _ = child.join();
            }
        });
        Ok(Self {
            endpoint,
            state,
            stop,
            thread: Some(thread),
        })
    }
    pub fn add(&self, record: Value) {
        self.state.lock().unwrap().records.push(record);
    }
}
impl Drop for FakeKibana {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
/// Deliberately use nanoseconds differing below an f64 ULP and repeated _doc
/// values across shards. The server keeps JSON integers exact.
pub fn record(id: usize) -> Value {
    let nanos = 1_793_620_800_123_456_789i64 + (id / 251) as i64;
    let stamp = time::OffsetDateTime::from_unix_timestamp_nanos(nanos as i128)
        .unwrap()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let mut source = json!({"@timestamp":stamp,"service":if id % 2 == 0 {"Checkout"} else {"Worker"},"level":"INFO","request_id":format!("request-{id}"),"details":format!("Synthetic investigation event {id}: a worker completed a read operation and emitted a structured result with full context."),"active":id % 2 == 0,"context":{"operation":"read"},"pod.info":{"name":"synthetic-pod"}});
    if id % 2 == 0 {
        source["duration"] = json!(id);
    }
    json!({"_index":if id % 2 == 0 {"remote-a:events"} else {"remote-b:events"},"_id":id.to_string(),"_source":source,"sort":[nanos,id % 37]})
}
/// Investigation fixture: repeated ids across hours, aliases and long text without message.
pub fn investigation_record(id: usize) -> Value {
    let mut event = record(id);
    let nanos = 1_793_491_200_000_000_000i64 + (id % 72) as i64 * 3_600_000_000_000;
    let stamp = time::OffsetDateTime::from_unix_timestamp_nanos(nanos as i128)
        .unwrap()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let source = event["_source"].as_object_mut().unwrap();
    source.remove("request_id");
    source.insert(
        if id % 4 == 0 {
            "request_id"
        } else {
            "trace_id"
        }
        .into(),
        json!(format!("request-{}", id % 3)),
    );
    source.insert("flow".into(), json!(format!("request-{}", id % 3)));
    source.insert(
        "details".into(),
        json!(format!(
            "Synthetic event {id}\n{}",
            "A read operation completed with structured investigation context. ".repeat(24)
        )),
    );
    source.insert("@timestamp".into(), json!(stamp));
    event["sort"][0] = json!(nanos);
    event
}

fn respond(mut stream: TcpStream, state: Arc<Mutex<State>>) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut first = String::new();
    reader.read_line(&mut first)?;
    let mut headers = BTreeMap::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if line == "\r\n" || line.is_empty() {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            headers.insert(key.to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    let size = headers
        .get("content-length")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    if size > 1024 * 1024 {
        return Ok(());
    }
    let mut bytes = vec![0; size];
    reader.read_exact(&mut bytes)?;
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let mut request = first.split_whitespace();
    let method = request.next().unwrap_or("");
    let path = request.next().unwrap_or("");
    let (status, response, delay) = {
        let mut s = state.lock().unwrap();
        s.calls.push(Call {
            method: method.into(),
            path: path.into(),
            body: body.clone(),
        });
        let required = [
            ("kbn-xsrf", "true"),
            ("elastic-api-version", "1"),
            ("x-elastic-internal-origin", "Kibana"),
        ];
        if !s.scenario.never_complete {
            s.pending.retain(|_, (_, left, _)| *left > 0);
        }
        let (status, response) = if headers.get("authorization").map(String::as_str)
            != Some("ApiKey test-secret")
        {
            (401, json!({"error":"authentication required"}))
        } else if required
            .iter()
            .any(|(k, v)| headers.get(*k).map(String::as_str) != Some(*v))
            || (method == "POST"
                && headers.get("content-type").map(String::as_str) != Some("application/json"))
        {
            s.rejected_headers += 1;
            (400, json!({"error":"required header missing"}))
        } else if path.ends_with("/api/status") {
            if s.scenario.status_denied {
                (403, json!({"error":"status unavailable"}))
            } else {
                (200, json!({"version":{"number":"10.1.2"}}))
            }
        } else if path.contains("/api/data_views/data_view/") {
            if s.scenario.data_view_denied {
                (403, json!({"error":"data view unavailable"}))
            } else {
                (
                    200,
                    json!({"data_view":{"title":"remote-*:events","timeFieldName":"@timestamp"}}),
                )
            }
        } else if path.ends_with("/internal/search/es") && method == "POST" {
            if !body["params"].is_object()
                || body["params"].get("body").is_some()
                || body["params"].get("pit").is_some()
                || !body["params"]["index"].is_string()
            {
                (400, json!({"error":"flat params with index required"}))
            } else if unsupported_kql(&body["params"]["query"]) {
                (
                    400,
                    json!({"error":"non-empty KQL is unsupported by the synthetic server"}),
                )
            } else if let Some(status) = s.scenario.failure_status {
                (status, json!({"error":"synthetic echo test-secret"}))
            } else if s.scenario.forbidden {
                (403, json!({"error":"permission denied"}))
            } else if s.scenario.unavailable {
                (503, json!({"error":"temporarily unavailable"}))
            } else if s.scenario.reject {
                (400, json!({"error":"query rejected"}))
            } else if s.scenario.malformed {
                (200, json!({"unexpected":true}))
            } else if let Some(id) = body["id"].as_str() {
                s.polls += 1;
                let never = s.scenario.never_complete;
                let partial_id = s.scenario.partial_with_id;
                let without_hits = s.scenario.running_without_hits;
                match s.pending.get_mut(id) {
                    Some((params, left, response)) if params == &body["params"] => {
                        *left = left.saturating_sub(1);
                        let running = never || *left > 0;
                        let mut response = response.clone();
                        response["isRunning"] = json!(running && !partial_id);
                        response["isPartial"] = json!(running || response["isPartial"] == true);
                        if running {
                            response["id"] = json!(id);
                            if without_hits {
                                response["rawResponse"]
                                    .as_object_mut()
                                    .unwrap()
                                    .remove("hits");
                            }
                        }
                        (200, response)
                    }
                    _ => (400, json!({"error":"unknown id or changed params"})),
                }
            } else {
                s.searches += 1;
                let mut response = search_response(&s.records, &body["params"], &s.scenario);
                if s.scenario.async_polls > 0 || s.scenario.never_complete {
                    s.next_id += 1;
                    let id = format!("async-{}", s.next_id);
                    let polls = s.scenario.async_polls;
                    s.pending.insert(
                        id.clone(),
                        (body["params"].clone(), polls, response.clone()),
                    );
                    response["id"] = json!(id);
                    response["isRunning"] = json!(!s.scenario.partial_with_id);
                    response["isPartial"] = json!(true);
                    if s.scenario.running_without_hits {
                        response["rawResponse"]
                            .as_object_mut()
                            .unwrap()
                            .remove("hits");
                    }
                }
                (200, response)
            }
        } else {
            (404, json!({"error":"route disabled"}))
        };
        (status, response, s.scenario.delay)
    };
    thread::sleep(delay);
    let bytes = serde_json::to_vec(&response).unwrap();
    write!(stream, "HTTP/1.1 {status} Response\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n", bytes.len())?;
    stream.write_all(&bytes)
}
fn field<'a>(source: &'a Value, path: &str) -> Option<&'a Value> {
    if let Some(value) = source.get(path) {
        return Some(value);
    }
    // _source may mix nested objects and literal dotted object keys.
    for (split, _) in path.match_indices('.').rev() {
        if let Some(parent) = source.get(&path[..split]) {
            if let Some(value) = field(parent, &path[split + 1..]) {
                return Some(value);
            }
        }
    }
    None
}

fn lookup(source: &Value, path: &str) -> Option<Value> {
    let (path, keyword) = path
        .strip_suffix(".keyword")
        .map_or((path, false), |p| (p, true));
    let value = if source.get("_source").is_some() {
        if matches!(path, "_index" | "_id") {
            source.get(path)
        } else {
            field(&source["_source"], path)
        }
    } else {
        field(source, path)
    };
    value.map(|v| {
        if keyword {
            v.as_str()
                .map(|s| json!(s.to_lowercase()))
                .unwrap_or_else(|| v.clone())
        } else {
            v.clone()
        }
    })
}
fn clauses(value: &Value) -> Vec<&Value> {
    value.as_array().map_or_else(
        || if value.is_null() { vec![] } else { vec![value] },
        |a| a.iter().collect(),
    )
}
fn unsupported_kql(query: &Value) -> bool {
    if let Some(kql) = query.get("kql") {
        return kql["query"]
            .as_str()
            .is_none_or(|text| !text.trim().is_empty());
    }
    query.get("bool").is_some_and(|b| {
        ["filter", "must", "must_not", "should"]
            .iter()
            .any(|key| clauses(&b[*key]).into_iter().any(unsupported_kql))
    })
}

fn matches(source: &Value, query: &Value) -> bool {
    if let Some(boolean) = query.get("bool") {
        return clauses(&boolean["filter"])
            .into_iter()
            .chain(clauses(&boolean["must"]))
            .all(|q| matches(source, q))
            && clauses(&boolean["must_not"])
                .into_iter()
                .all(|q| !matches(source, q))
            && (boolean.get("should").is_none()
                || clauses(&boolean["should"])
                    .into_iter()
                    .filter(|q| matches(source, q))
                    .count()
                    >= boolean["minimum_should_match"].as_u64().unwrap_or(1) as usize);
    }
    if let Some(range) = query["range"].as_object() {
        return range.iter().all(|(path, limits)| {
            let Some(actual) = lookup(source, path) else {
                return false;
            };
            limits
                .as_object()
                .unwrap()
                .iter()
                .filter(|(op, _)| op.as_str() != "format")
                .all(|(op, limit)| {
                    let cmp = compare(&actual, limit);
                    match op.as_str() {
                        "gte" => !cmp.is_lt(),
                        "gt" => cmp.is_gt(),
                        "lt" => cmp.is_lt(),
                        "lte" => !cmp.is_gt(),
                        _ => false,
                    }
                })
        });
    }
    if let Some(path) = query["exists"]["field"].as_str() {
        return lookup(source, path).is_some_and(|v| !v.is_null());
    }
    for kind in ["term", "terms", "match_phrase", "match"] {
        if let Some(object) = query[kind].as_object() {
            return object.iter().all(|(path, expected)| {
                let Some(actual) = lookup(source, path) else {
                    return false;
                };
                let expected = if path.ends_with(".keyword") {
                    expected
                        .as_str()
                        .map(|s| json!(s.to_lowercase()))
                        .unwrap_or_else(|| expected.clone())
                } else {
                    expected.clone()
                };
                if kind == "terms" {
                    expected.as_array().is_some_and(|a| a.contains(&actual))
                } else {
                    actual == expected
                }
            });
        }
    }
    query.get("match_all").is_some()
        || query.get("kql").is_some_and(|q| {
            q["query"]
                .as_str()
                .is_some_and(|text| text.trim().is_empty())
        })
}
fn compare(a: &Value, b: &Value) -> std::cmp::Ordering {
    if let (Some(a), Some(b)) = (a.as_i64(), b.as_i64()) {
        return a.cmp(&b);
    }
    if let (Some(a), Some(b)) = (a.as_u64(), b.as_u64()) {
        return a.cmp(&b);
    }
    if let (Some(a), Some(b)) = (a.as_str(), b.as_str()) {
        let parse =
            |s| time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339);
        if let (Ok(a), Ok(b)) = (parse(a), parse(b)) {
            return a.cmp(&b);
        }
        return a.cmp(b);
    }
    a.to_string().cmp(&b.to_string())
}
fn sort_cmp(a: &Value, b: &Value, sort: &[Value]) -> std::cmp::Ordering {
    for (i, spec) in sort.iter().enumerate() {
        let order = spec.as_object().unwrap().values().next().unwrap();
        let desc = order == "desc" || order["order"] == "desc";
        let cmp = compare(&a[i], &b[i]);
        if !cmp.is_eq() {
            return if desc { cmp.reverse() } else { cmp };
        }
    }
    std::cmp::Ordering::Equal
}
fn search_response(records: &[Value], params: &Value, scenario: &Scenario) -> Value {
    let mut included = Vec::new();
    let mut excluded = Vec::new();
    for pattern in params["index"].as_str().unwrap().split(',') {
        let (negative, pattern) = pattern
            .strip_prefix('-')
            .map_or((false, pattern), |p| (true, p));
        let Ok(glob) = globset::Glob::new(if pattern == "_all" { "*" } else { pattern }) else {
            return json!({"error":"unsupported synthetic index pattern"});
        };
        if negative {
            excluded.push(glob.compile_matcher());
        } else {
            included.push(glob.compile_matcher());
        }
    }
    let mut rows: Vec<_> = records
        .iter()
        .filter(|r| {
            let index = r["_index"].as_str().unwrap();
            (included.is_empty() || included.iter().any(|g| g.is_match(index)))
                && !excluded.iter().any(|g| g.is_match(index))
                && matches(r, &params["query"])
        })
        .cloned()
        .collect();
    let total = rows.len();
    let cap = params["track_total_hits"].as_u64();
    let bound = scenario.lower_bound || cap.is_some_and(|c| total as u64 >= c);
    let reported = cap.map_or(total as u64, |c| c.min(total as u64));
    let hit_total = if scenario.legacy_total {
        json!(reported)
    } else {
        json!({"value":reported,"relation":if bound {"gte"} else {"eq"}})
    };
    let mut raw = json!({"timed_out":scenario.timed_out,"_shards":{"total":97,"successful":if scenario.partial {96} else {97},"skipped":91,"failed":if scenario.partial {1} else {0}},"_clusters":{"total":2,"successful":if scenario.partial {1} else {2},"skipped":0,"running":0,"partial":if scenario.partial {1} else {0},"failed":0,"details":{"remote-a":{"status":"successful","_shards":{"total":49,"successful":49,"skipped":46,"failed":0}},"remote-b":{"status":if scenario.partial {"partial"} else {"successful"}}}},"hits":{"total":hit_total,"hits":[]}});
    if scenario.partial {
        raw["_shards"]["failures"] = json!([{"reason":{"type":"synthetic_failure"}}]);
    }
    if let Some(aggs) = params["aggs"].as_object() {
        let name = aggs["values"]["terms"]["field"].as_str().unwrap();
        let limit = aggs["values"]["terms"]["size"].as_u64().unwrap() as usize;
        let mut values: BTreeMap<String, (Value, u64)> = BTreeMap::new();
        let mut missing = 0;
        for row in &rows {
            match lookup(row, name).filter(|v| !v.is_null()) {
                Some(value) => {
                    values.entry(value.to_string()).or_insert((value, 0)).1 += 1;
                }
                None => missing += 1,
            }
        }
        let mut values: Vec<_> = values.into_values().collect();
        values.sort_by(|a, b| {
            b.1.cmp(&a.1).then_with(|| {
                if a.0.is_number() && b.0.is_number() {
                    compare(&a.0, &b.0)
                } else {
                    a.0.to_string().cmp(&b.0.to_string())
                }
            })
        });
        let other: u64 = values.iter().skip(limit).map(|v| v.1).sum();
        let buckets: Vec<_> = values
            .into_iter()
            .take(limit)
            .map(|(key, count)| match key {
                Value::Bool(b) => {
                    json!({"key":if b {1} else {0},"key_as_string":b.to_string(),"doc_count":count})
                }
                _ => json!({"key":key,"doc_count":count}),
            })
            .collect();
        raw["aggregations"] = json!({"values":{"buckets":buckets,"sum_other_doc_count":other,"doc_count_error_upper_bound":0},"missing_values":{"doc_count":missing}});
    } else {
        let sort = params["sort"].as_array().unwrap();
        rows.sort_by(|a, b| sort_cmp(&a["sort"], &b["sort"], sort));
        if let Some(after) = params.get("search_after") {
            rows.retain(|r| sort_cmp(&r["sort"], after, sort).is_gt());
        }
        let from = params["from"].as_u64().unwrap_or(0) as usize;
        let size = params["size"].as_u64().unwrap() as usize;
        raw["hits"]["hits"] = json!(rows.into_iter().skip(from).take(size).collect::<Vec<_>>());
    }
    if scenario.omit_completion_flags {
        json!({"rawResponse":raw})
    } else {
        json!({"isRunning":false,"isPartial":scenario.partial,"rawResponse":raw})
    }
}
