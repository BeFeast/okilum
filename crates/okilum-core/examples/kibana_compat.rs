//! Bounded Kibana compatibility runner (#1440).
//!
//! Drives the product's internal-search connector (`remote_log::transport` and
//! `remote_log::live`) against a real Kibana + Elasticsearch pair that
//! `scripts/ci/kibana-compat.py` starts from digest-pinned images on a
//! GitHub-hosted runner. It seeds synthetic documents only and authenticates
//! with an ephemeral, read-only API key minted inside that container. Every
//! endpoint must be a literal loopback IP: this binary cannot address a real or
//! corporate deployment. Debug builds only, like `fake_kibana`, because HTTP
//! sources require debug assertions.
//!
//! Environment (set by the orchestrator): OKC_KIBANA_URL, OKC_ES_URL,
//! OKC_ES_PASSWORD (the container's ephemeral `elastic` password, used only to
//! seed and to insert the late event), OKC_API_KEY, OKC_DATA_VIEW, OKC_VERSION,
//! OKC_REPORT. Exit status: 0 all checks passed or were reported as
//! unsupported/informational, 1 a check failed, 2 setup error, 3 run budget.

#[cfg(debug_assertions)]
fn main() {
    let code = match compat::run() {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(error) => {
            eprintln!("kibana_compat: {error:#}");
            2
        }
    };
    std::process::exit(code);
}

#[cfg(not(debug_assertions))]
fn main() {
    eprintln!("The Kibana compatibility runner is available only in debug builds");
    std::process::exit(2);
}

#[cfg(debug_assertions)]
mod compat {
    use anyhow::{bail, ensure, Context, Result};
    use okilum_core::remote_log::{
        discover::TimeRange,
        live::{Mode, Status, Update, Worker},
        result::{AuthError, FieldType, FieldValue, FieldValuesRequest, TotalRelation},
        transport::{ApiKey, History, Kibana},
        Source,
    };
    use serde_json::{json, Value};
    use std::{
        collections::{BTreeMap, BTreeSet},
        sync::{atomic::AtomicBool, mpsc, Arc},
        time::{Duration, Instant},
    };
    use time::{format_description::well_known::Rfc3339, OffsetDateTime};

    const SEC: i128 = 1_000_000_000;
    /// 2026-11-01T00:00:00Z.
    const BASE: i128 = 1_793_491_200 * SEC;
    const DOCS: usize = 600;
    /// Documents in [TIE_FROM, TIE_TO) share one nanosecond timestamp. They span
    /// five shards and the 200-row page boundary, which exercises boundary replay.
    const TIE_FROM: usize = 150;
    const TIE_TO: usize = 400;
    const PATTERN: &str = "okilum-compat-*";
    const CONFLICT_PATTERN: &str = "okilum-conflict-*";
    const LATE_ID: &str = "late-0001";
    /// Whole-process bound; the orchestrator kills the process later still.
    const RUN_BUDGET: Duration = Duration::from_secs(240);
    /// Live phase bound, from Apply to the late event becoming visible.
    const LATE_BUDGET: Duration = Duration::from_secs(60);

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Outcome {
        Pass,
        Fail,
        /// The server rejected the capability explicitly (no silent rows).
        Unsupported,
        /// Observed and reported; neither establishes nor refutes the contract.
        Info,
    }
    impl Outcome {
        fn name(self) -> &'static str {
            match self {
                Self::Pass => "pass",
                Self::Fail => "fail",
                Self::Unsupported => "unsupported",
                Self::Info => "info",
            }
        }
    }

    struct Doc {
        id: String,
        index: &'static str,
        nanos: i128,
        service: &'static str,
        level: &'static str,
        phrase: bool,
        status: Option<u64>,
        flag: bool,
        seq: usize,
    }

    fn doc(i: usize) -> Doc {
        let nanos = if (TIE_FROM..TIE_TO).contains(&i) {
            BASE + 150 * SEC + 123_456_789
        } else {
            // Nine significant sub-second digits, distinct per document.
            BASE + i as i128 * SEC + 100_000_001 + (i as i128 * 7_919) % 800_000_000
        };
        Doc {
            id: format!("doc-{i:04}"),
            index: if i.is_multiple_of(2) {
                "okilum-compat-a"
            } else {
                "okilum-compat-b"
            },
            nanos,
            service: ["checkout", "auth", "search", "billing"][i % 4],
            level: ["info", "warn", "error"][i % 3],
            phrase: i.is_multiple_of(10),
            status: (!i.is_multiple_of(5)).then_some([200, 404, 500][(i / 2) % 3]),
            flag: i % 7 < 3,
            seq: i,
        }
    }

    fn stamp(nanos: i128) -> Result<String> {
        Ok(OffsetDateTime::from_unix_timestamp_nanos(nanos)?.format(&Rfc3339)?)
    }

    fn body(d: &Doc) -> Result<Value> {
        let message = if d.phrase {
            format!("payment declined for order {}", d.id)
        } else {
            format!("request handled for order {}", d.id)
        };
        let mut value = json!({
            "@timestamp": stamp(d.nanos)?,
            "service": {"name": d.service},
            "level": d.level,
            "message": message,
            "flag": d.flag,
            "seq": d.seq,
        });
        if let Some(status) = d.status {
            value["http"] = json!({ "status": status });
        }
        Ok(value)
    }

    type Identity = (String, String);

    fn expected(keep: fn(&Doc) -> bool) -> BTreeSet<Identity> {
        (0..DOCS)
            .map(doc)
            .filter(keep)
            .map(|d| (d.index.to_owned(), d.id))
            .collect()
    }

    /// Seeding and the late insert only; the product transport talks to Kibana.
    struct Elasticsearch {
        client: reqwest::blocking::Client,
        base: url::Url,
        password: String,
    }
    impl Elasticsearch {
        fn call(
            &self,
            method: reqwest::Method,
            path: &str,
            body: String,
            kind: &str,
        ) -> Result<Value> {
            let response = self
                .client
                .request(method, self.base.join(path)?)
                .basic_auth("elastic", Some(&self.password))
                .header("Content-Type", kind)
                .body(body)
                .send()
                .with_context(|| format!("Elasticsearch {path}: request failed"))?;
            let status = response.status();
            let text = response.text().unwrap_or_default();
            ensure!(status.is_success(), "Elasticsearch {path}: HTTP {status}");
            Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
        }
        fn json(&self, method: reqwest::Method, path: &str, body: &Value) -> Result<Value> {
            self.call(method, path, body.to_string(), "application/json")
        }
    }

    /// Diagnostic only: the HTTP status and Kibana's error message for a route,
    /// with the product's headers, so a failed check names its server-side cause.
    /// Never prints credentials or response rows.
    fn probe(es: &Elasticsearch, kibana: &url::Url, path: &str, api_key: &str) -> String {
        probe_route(es, kibana, path, api_key)
            .unwrap_or_else(|error| format!("probe failed: {error:#}"))
    }
    fn probe_route(
        es: &Elasticsearch,
        kibana: &url::Url,
        path: &str,
        api_key: &str,
    ) -> Result<String> {
        let response = es
            .client
            .get(kibana.join(path)?)
            .header("Authorization", format!("ApiKey {api_key}"))
            .header("kbn-xsrf", "true")
            .header("x-elastic-internal-origin", "Kibana")
            .header("elastic-api-version", "1")
            .send()?;
        let status = response.status().as_u16();
        let body: Value = serde_json::from_str(&response.text()?).unwrap_or(Value::Null);
        let message = match body["message"].as_str() {
            Some(message) => message.chars().take(200).collect::<String>(),
            None => {
                let keys: Vec<&String> = body
                    .as_object()
                    .into_iter()
                    .flat_map(|o| o.keys())
                    .collect();
                format!("keys {keys:?}")
            }
        };
        Ok(format!("HTTP {status}: {message}"))
    }

    fn seed(es: &Elasticsearch) -> Result<()> {
        let mapping = json!({"dynamic": "strict", "properties": {
            "@timestamp": {"type": "date_nanos"},
            "service": {"properties": {"name": {"type": "keyword"}}},
            "level": {"type": "keyword"},
            "message": {"type": "text"},
            "http": {"properties": {"status": {"type": "long"}}},
            "flag": {"type": "boolean"},
            "seq": {"type": "long"},
        }});
        for (index, shards) in [("okilum-compat-a", 3), ("okilum-compat-b", 2)] {
            es.json(
                reqwest::Method::PUT,
                index,
                &json!({"settings": {"number_of_shards": shards, "number_of_replicas": 0}, "mappings": mapping}),
            )?;
        }
        // The same field mapped as keyword in one index and long in the other:
        // a term query for a non-numeric value fails the long index's shard.
        for (index, kind) in [
            ("okilum-conflict-k", "keyword"),
            ("okilum-conflict-n", "long"),
        ] {
            es.json(
                reqwest::Method::PUT,
                index,
                &json!({"settings": {"number_of_shards": 1, "number_of_replicas": 0},
                        "mappings": {"properties": {"@timestamp": {"type": "date_nanos"}, "code": {"type": kind}}}}),
            )?;
        }
        let mut bulk = String::new();
        let mut line = |value: Value| {
            bulk.push_str(&value.to_string());
            bulk.push('\n');
        };
        for i in 0..DOCS {
            let d = doc(i);
            line(json!({"index": {"_index": d.index, "_id": d.id}}));
            line(body(&d)?);
        }
        for i in 0..3 {
            let at = stamp(BASE + i * SEC)?;
            line(
                json!({"index": {"_index": "okilum-conflict-k", "_id": format!("conflict-k-{i}")}}),
            );
            line(json!({"@timestamp": at, "code": "abc"}));
            line(
                json!({"index": {"_index": "okilum-conflict-n", "_id": format!("conflict-n-{i}")}}),
            );
            line(json!({"@timestamp": at, "code": 7}));
        }
        let response = es.call(
            reqwest::Method::POST,
            "_bulk?refresh=wait_for",
            bulk,
            "application/x-ndjson",
        )?;
        ensure!(
            response["errors"] == false,
            "Bulk seeding reported item errors"
        );
        Ok(())
    }

    struct Traversal {
        events: Vec<Value>,
        status: okilum_core::remote_log::result::ResultStatus,
        complete: bool,
        pages: usize,
    }

    fn traverse(kibana: &Kibana, source: &Source) -> Result<Traversal> {
        let mut history = History::for_source(kibana.clone(), source)?;
        let mut events = Vec::new();
        let mut pages = 0;
        loop {
            let page = history.page()?;
            pages += 1;
            events.extend(page.events);
            if !history.can_page() {
                return Ok(Traversal {
                    events,
                    status: page.status,
                    complete: page.complete,
                    pages,
                });
            }
            ensure!(pages < 64, "History did not finish within 64 pages");
        }
    }

    fn identity(event: &Value) -> Result<Identity> {
        Ok((
            event["_index"].as_str().context("Row lacks _index")?.into(),
            event["_id"].as_str().context("Row lacks _id")?.into(),
        ))
    }

    fn expect_rows(events: &[Value], expected: &BTreeSet<Identity>) -> Result<()> {
        let ids = events.iter().map(identity).collect::<Result<Vec<_>>>()?;
        let unique: BTreeSet<_> = ids.iter().cloned().collect();
        ensure!(
            unique.len() == ids.len(),
            "{} duplicate rows",
            ids.len() - unique.len()
        );
        let missing = expected.difference(&unique).count();
        let extra = unique.difference(expected).count();
        ensure!(
            missing == 0 && extra == 0,
            "{missing} expected rows missing, {extra} unexpected rows"
        );
        Ok(())
    }

    fn expect_clean(t: &Traversal) -> Result<()> {
        ensure!(
            t.complete && !t.status.incomplete(),
            "Traversal incomplete: {:?}",
            t.status
        );
        Ok(())
    }

    fn event_nanos(event: &Value) -> Result<i128> {
        let text = event["fields"]["@timestamp"][0]
            .as_str()
            .context("Row lacks the formatted @timestamp field")?;
        Ok(OffsetDateTime::parse(text, &Rfc3339)
            .with_context(|| format!("Unparseable @timestamp {text}"))?
            .unix_timestamp_nanos())
    }

    /// Exact set, ordering and nanosecond preservation of one full traversal.
    fn history(kibana: &Kibana, source: &Source, ascending: bool) -> Result<(Outcome, String)> {
        let t = traverse(kibana, source)?;
        expect_clean(&t)?;
        expect_rows(&t.events, &expected(|_| true))?;
        let stamps: BTreeMap<String, i128> = (0..DOCS).map(doc).map(|d| (d.id, d.nanos)).collect();
        let mut previous: Option<i128> = None;
        for event in &t.events {
            let id = event["_id"].as_str().unwrap_or_default();
            let nanos = event_nanos(event)?;
            ensure!(
                stamps.get(id) == Some(&nanos),
                "{id}: nanosecond timestamp changed in transit"
            );
            if let Some(previous) = previous {
                ensure!(
                    if ascending {
                        nanos >= previous
                    } else {
                        nanos <= previous
                    },
                    "{id}: rows out of server sort order"
                );
            }
            previous = Some(nanos);
        }
        let total = t.status.total.as_ref().map(|t| (t.value, t.relation));
        ensure!(
            total == Some((DOCS as u64, TotalRelation::Eq)),
            "Unexpected hit total {total:?}"
        );
        ensure!(
            t.status.total_shards.is_some() && t.status.total_shards == t.status.successful_shards,
            "Shards {:?}/{:?}",
            t.status.successful_shards,
            t.status.total_shards
        );
        Ok((
            Outcome::Pass,
            format!(
                "{} rows in {} requests, exact total, {}/{} shards, {} tied rows across the page boundary, nanoseconds preserved",
                t.events.len(),
                t.pages,
                t.status.successful_shards.unwrap_or(0),
                t.status.total_shards.unwrap_or(0),
                TIE_TO - TIE_FROM
            ),
        ))
    }

    fn with_query(base: &Source, kql: &str) -> Source {
        let mut source = base.clone();
        source.discover.query.query = kql.into();
        source
    }
    fn with_filters(base: &Source, pattern: &str, filters: Vec<Value>) -> Source {
        let mut source = base.clone();
        source.index_pattern = Some(pattern.into());
        source.discover.filters = filters;
        source
    }

    /// Server-side KQL against the equivalent DSL filter on the same seed.
    fn kql(
        kibana: &Kibana,
        base: &Source,
        query: &str,
        filters: Vec<Value>,
        keep: fn(&Doc) -> bool,
    ) -> Result<(Outcome, String)> {
        let want = expected(keep);
        ensure!(!want.is_empty(), "Empty synthetic expectation");
        let dsl = traverse(kibana, &with_filters(base, PATTERN, filters))?;
        expect_clean(&dsl)?;
        expect_rows(&dsl.events, &want).context("DSL baseline")?;
        match traverse(kibana, &with_query(base, query)) {
            Ok(t) => {
                expect_clean(&t)?;
                expect_rows(&t.events, &want).context("KQL disagrees with DSL")?;
                Ok((
                    Outcome::Pass,
                    format!("KQL `{query}` and DSL return the same {} rows", want.len()),
                ))
            }
            Err(error) => Ok((
                Outcome::Unsupported,
                format!(
                    "KQL `{query}` rejected explicitly ({error:#}); DSL baseline {} rows",
                    want.len()
                ),
            )),
        }
    }

    fn invalid_kql(kibana: &Kibana, base: &Source) -> Result<(Outcome, String)> {
        match traverse(kibana, &with_query(base, "service.name : (")) {
            Err(error) => Ok((
                Outcome::Pass,
                format!("invalid KQL rejected explicitly: {error:#}"),
            )),
            Ok(t) => bail!(
                "invalid KQL returned {} rows instead of an error",
                t.events.len()
            ),
        }
    }

    fn shard_failure(kibana: &Kibana, base: &Source) -> Result<(Outcome, String)> {
        let source = with_filters(
            base,
            CONFLICT_PATTERN,
            vec![json!({"meta": {}, "query": {"term": {"code": "abc"}}})],
        );
        match traverse(kibana, &source) {
            Err(error) => Ok((Outcome::Pass, format!("explicit error: {error:#}"))),
            Ok(t) if t.status.failed_shards > 0 => {
                ensure!(
                    t.status.incomplete() && !t.complete,
                    "{} failed shards but the result is labelled complete",
                    t.status.failed_shards
                );
                Ok((
                    Outcome::Pass,
                    format!(
                        "partial result visible: {} rows, {} failed of {:?} shards",
                        t.events.len(),
                        t.status.failed_shards,
                        t.status.total_shards
                    ),
                ))
            }
            Ok(t) => {
                ensure!(
                    t.events.len() == 3,
                    "silent loss: {} rows, no failed shards reported",
                    t.events.len()
                );
                Ok((
                    Outcome::Info,
                    "server produced no shard failure; all 3 keyword rows returned".into(),
                ))
            }
        }
    }

    fn field_values(kibana: &Kibana, base: &Source) -> Result<(Outcome, String)> {
        let docs: Vec<Doc> = (0..DOCS).map(doc).collect();
        let request = |field: &str, field_type| FieldValuesRequest {
            field: field.into(),
            limit: 10,
            field_type,
        };
        let count =
            |values: &okilum_core::remote_log::result::FieldValues| -> BTreeMap<String, u64> {
                values
                    .buckets
                    .iter()
                    .map(|bucket| {
                        let key = match &bucket.value {
                            FieldValue::String(text) => text.clone(),
                            FieldValue::Number(number) => number.to_string(),
                            FieldValue::Boolean(flag) => flag.to_string(),
                            FieldValue::Missing => "<missing>".into(),
                        };
                        (key, bucket.count)
                    })
                    .collect()
            };
        let tally = |key: &dyn Fn(&Doc) -> Option<String>| {
            let mut map = BTreeMap::new();
            for d in &docs {
                if let Some(k) = key(d) {
                    *map.entry(k).or_insert(0u64) += 1;
                }
            }
            map
        };
        let cases: [(&str, FieldType, BTreeMap<String, u64>, u64); 3] = [
            (
                "service.name",
                FieldType::String,
                tally(&|d| Some(d.service.into())),
                0,
            ),
            (
                "http.status",
                FieldType::Number,
                tally(&|d| d.status.map(|s| s.to_string())),
                docs.iter().filter(|d| d.status.is_none()).count() as u64,
            ),
            (
                "flag",
                FieldType::Boolean,
                tally(&|d| Some(d.flag.to_string())),
                0,
            ),
        ];
        for (field, field_type, want, missing) in cases {
            let values = kibana.field_values(base, &request(field, field_type))?;
            ensure!(
                !values.status.incomplete(),
                "{field}: incomplete {:?}",
                values.status
            );
            let got = count(&values);
            ensure!(got == want, "{field}: buckets {got:?}, expected {want:?}");
            ensure!(
                values.missing_count == missing,
                "{field}: missing {} expected {missing}",
                values.missing_count
            );
        }
        Ok((
            Outcome::Pass,
            "keyword, long (with missing) and boolean top-N buckets match the seed".into(),
        ))
    }

    fn unauthorized(base: &Source) -> Result<(Outcome, String)> {
        // base64("okilum:invalid"): well-formed, unknown to the server.
        let key = ApiKey::new(b"b2tpbHVtOmludmFsaWQ=".to_vec())?;
        let bad = Kibana::new(base, key, Arc::new(AtomicBool::new(false)))?;
        match traverse(&bad, base) {
            Err(error) => {
                let status = error.downcast_ref::<AuthError>().map(|a| a.status);
                ensure!(
                    status == Some(401),
                    "expected typed HTTP 401, got {error:#}"
                );
                Ok((
                    Outcome::Pass,
                    "unknown API key surfaces as typed AuthError 401".into(),
                ))
            }
            Ok(t) => bail!("unknown key returned {} rows", t.events.len()),
        }
    }

    fn wait_for(
        updates: &mpsc::Receiver<Update>,
        deadline: Instant,
        mut accept: impl FnMut(&Update) -> bool,
    ) -> Result<Update> {
        loop {
            let left = deadline
                .checked_duration_since(Instant::now())
                .context("Bounded live wait expired")?;
            let update = updates
                .recv_timeout(left)
                .context("No live update before the deadline")?;
            match &update.status {
                Status::Error(error) => bail!("Worker error: {error}"),
                Status::AuthError(auth) => {
                    bail!("Worker authentication error: HTTP {}", auth.status)
                }
                Status::TimedOut => bail!("Worker timed out"),
                Status::IndexPatternRequired => bail!("Worker requires an index pattern"),
                Status::Incomplete { reason, .. } => bail!("Worker incomplete: {reason}"),
                Status::Reconnecting { error, .. } => bail!("Worker reconnecting: {error}"),
                _ => {}
            }
            if accept(&update) {
                return Ok(update);
            }
        }
    }

    /// Fixed-range following includes an event that arrives late: its event
    /// time lies inside the window, before rows already loaded.
    fn late_arrival(
        es: &Elasticsearch,
        base: &Source,
        key: &ApiKey,
        range: &TimeRange,
    ) -> Result<(Outcome, String)> {
        let (worker, receiver) = Worker::start();
        let (sender, updates) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(update) = receiver.recv_blocking() {
                if sender.send(update).is_err() {
                    break;
                }
            }
        });
        let deadline = Instant::now() + LATE_BUDGET;
        worker.apply(base.clone(), key.clone());
        wait_for(&updates, deadline, |u| {
            matches!(u.status, Status::History { .. })
        })?;
        worker.start_live(Mode::Fixed(range.clone()), base);
        let first = wait_for(&updates, deadline, |u| {
            matches!(u.status, Status::Complete { .. })
        })?;
        let rows = first
            .loaded
            .as_ref()
            .context("Live cycle without rows")?
            .window
            .events
            .len();
        ensure!(
            rows == DOCS,
            "First live cycle loaded {rows} rows, expected {DOCS}"
        );
        let late = Doc {
            id: LATE_ID.into(),
            index: "okilum-compat-a",
            nanos: BASE + 10 * SEC + 5,
            service: "checkout",
            level: "warn",
            phrase: false,
            status: Some(200),
            flag: true,
            seq: DOCS,
        };
        es.json(
            reqwest::Method::PUT,
            &format!("okilum-compat-a/_doc/{LATE_ID}?refresh=wait_for"),
            &body(&late)?,
        )?;
        // refresh=wait_for returns once the document is searchable.
        let searchable = Instant::now();
        let mut cycles = 0;
        let seen = wait_for(&updates, deadline, |u| {
            if !matches!(u.status, Status::Complete { .. }) {
                return false;
            }
            cycles += 1;
            u.loaded
                .as_ref()
                .is_some_and(|l| l.window.events.iter().any(|e| e["_id"] == LATE_ID))
        })?;
        let latency = searchable.elapsed();
        let events = &seen.loaded.as_ref().context("No rows")?.window.events;
        let unique: BTreeSet<Identity> = events
            .iter()
            .map(|e| identity(e.as_ref()))
            .collect::<Result<_>>()?;
        ensure!(
            unique.len() == events.len() && events.len() == DOCS + 1,
            "Live window holds {} rows ({} unique), expected {}",
            events.len(),
            unique.len(),
            DOCS + 1
        );
        ensure!(seen.following, "Live following stopped");
        worker.stop();
        drop(worker);
        Ok((
            Outcome::Pass,
            format!(
                "late event inside the fixed window appeared {:.2}s after it became searchable ({cycles} complete cycles, {} rows, no duplicates); synthetic loopback data, not a production latency claim",
                latency.as_secs_f64(),
                DOCS + 1
            ),
        ))
    }

    #[derive(Default)]
    struct Report {
        checks: Vec<Value>,
        failed: bool,
    }
    impl Report {
        fn check(&mut self, name: &str, run: impl FnOnce() -> Result<(Outcome, String)>) {
            let began = Instant::now();
            let (outcome, detail) =
                run().unwrap_or_else(|error| (Outcome::Fail, format!("{error:#}")));
            let seconds = (began.elapsed().as_secs_f64() * 1000.0).round() / 1000.0;
            self.failed |= outcome == Outcome::Fail;
            println!(
                "{name:<28} {:<11} {seconds:>7.3}s  {detail}",
                outcome.name()
            );
            self.checks.push(json!({"name": name, "outcome": outcome.name(), "seconds": seconds, "detail": detail}));
        }
    }

    fn loopback(text: &str) -> Result<url::Url> {
        let url = url::Url::parse(text)?;
        let local = match url.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            _ => false,
        };
        ensure!(
            local && url.scheme() == "http",
            "Endpoints must be literal loopback HTTP: this runner targets only containers it started"
        );
        Ok(url)
    }

    pub fn run() -> Result<bool> {
        std::thread::spawn(|| {
            std::thread::sleep(RUN_BUDGET);
            eprintln!("kibana_compat: run budget of {RUN_BUDGET:?} exceeded");
            std::process::exit(3);
        });
        let env = |name: &str| std::env::var(name).with_context(|| format!("{name} is not set"));
        let kibana_url = env("OKC_KIBANA_URL")?;
        let kibana_base = loopback(&kibana_url)?;
        let es = Elasticsearch {
            client: reqwest::blocking::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(30))
                .build()?,
            base: loopback(&env("OKC_ES_URL")?)?,
            password: env("OKC_ES_PASSWORD")?,
        };
        let api_key = env("OKC_API_KEY")?;
        let key = ApiKey::new(api_key.clone().into_bytes())?;
        let data_view = env("OKC_DATA_VIEW")?;
        let version = env("OKC_VERSION")?;
        let report_path = env("OKC_REPORT")?;

        seed(&es).context("Seeding synthetic documents")?;
        let range = TimeRange {
            from: stamp(BASE)?,
            to: stamp(BASE + 3600 * SEC)?,
        };
        let mut base = Source::direct("kibana-compat", &kibana_url, PATTERN, "@timestamp")?;
        base.discover.time = range.clone();
        base.validate()?;
        let kibana = Kibana::new(&base, key.clone(), Arc::new(AtomicBool::new(false)))?;

        let mut report = Report::default();
        report.check("data_view_resolution", || {
            let mut legacy = base.clone();
            legacy.index_pattern = None;
            legacy.discover.data_view = data_view.clone();
            let resolved = kibana.resolve_source(&legacy).with_context(|| {
                probe(
                    &es,
                    &kibana_base,
                    &format!("api/data_views/data_view/{data_view}"),
                    &api_key,
                )
            })?;
            ensure!(
                resolved.index_pattern.as_deref() == Some(PATTERN)
                    && resolved.time_field == "@timestamp",
                "Resolved {:?} / {}",
                resolved.index_pattern,
                resolved.time_field
            );
            Ok((
                Outcome::Pass,
                format!("data view `{data_view}` resolves to {PATTERN} on @timestamp"),
            ))
        });
        report.check("kibana_version", || {
            // The first page carries the optional /api/status version.
            let page = History::for_source(kibana.clone(), &base)?.page()?;
            match page.status.version {
                Some(seen) if seen == version => {
                    Ok((Outcome::Pass, format!("/api/status reports {seen}")))
                }
                Some(seen) => bail!("/api/status reports {seen}, image is {version}"),
                None => Ok((
                    Outcome::Info,
                    format!(
                        "/api/status version not readable with the read-only key ({})",
                        probe(&es, &kibana_base, "api/status", &api_key)
                    ),
                )),
            }
        });
        report.check("history_ascending", || history(&kibana, &base, true));
        report.check("history_descending", || {
            let mut source = base.clone();
            source.discover.sort[0].order = "desc".into();
            history(&kibana, &source, false)
        });
        report.check("kql_keyword", || {
            kql(
                &kibana,
                &base,
                r#"service.name : "checkout""#,
                vec![json!({"meta": {}, "query": {"term": {"service.name": "checkout"}}})],
                |d| d.service == "checkout",
            )
        });
        report.check("kql_conjunction_negation", || {
            kql(
                &kibana,
                &base,
                r#"level : "error" and not service.name : "auth""#,
                vec![
                    json!({"meta": {}, "query": {"term": {"level": "error"}}}),
                    json!({"meta": {"negate": true}, "query": {"term": {"service.name": "auth"}}}),
                ],
                |d| d.level == "error" && d.service != "auth",
            )
        });
        report.check("kql_phrase", || {
            kql(
                &kibana,
                &base,
                r#"message : "payment declined""#,
                vec![
                    json!({"meta": {}, "query": {"match_phrase": {"message": "payment declined"}}}),
                ],
                |d| d.phrase,
            )
        });
        report.check("kql_invalid", || invalid_kql(&kibana, &base));
        report.check("shard_failure", || shard_failure(&kibana, &base));
        report.check("field_values", || field_values(&kibana, &base));
        report.check("unauthorized", || unauthorized(&base));
        report.check("late_arrival", || late_arrival(&es, &base, &key, &range));

        let document = json!({
            "schema": 1,
            "synthetic": true,
            "version": version,
            "documents": DOCS,
            "checks": report.checks,
        });
        std::fs::write(&report_path, serde_json::to_vec_pretty(&document)?)
            .with_context(|| format!("Writing {report_path}"))?;
        Ok(!report.failed)
    }
}
