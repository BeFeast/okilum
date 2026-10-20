use super::*;
use crate::remote_log::{tests::quality, MAX_ROWS, PAGE_ROWS};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    thread,
};

#[derive(Default)]
struct State {
    visible: Vec<Value>,
    pits: HashMap<String, Vec<Value>>,
    opens: usize,
    searches: usize,
    closes: Vec<String>,
    cursors: Vec<Value>,
    late_on_pit: Option<usize>,
    fail_next: bool,
    fail_close: bool,
    partial_next: bool,
    slow_next: bool,
    active: usize,
    peak: usize,
    delay: Duration,
}
struct Server {
    endpoint: url::Url,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    slow_started: Arc<AtomicBool>,
    disconnected: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.thread.take().unwrap().join().unwrap();
    }
}
fn hit(id: usize) -> Value {
    let timestamp = OffsetDateTime::parse(
        "2026-10-20T12:00:00.123456789Z",
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap()
        - time::Duration::seconds((id / 2) as i64);
    json!({"_index":"main:log-2","_id":id.to_string(),"_source":{"@timestamp":timestamp.format(&time::format_description::well_known::Rfc3339).unwrap(),"description":"event","all":{"nested":[1,2]}},"sort":[timestamp.format(&time::format_description::well_known::Rfc3339).unwrap(),9223372036854775807u64-id as u64]})
}
fn source() -> Source {
    let mut source = Source::import(
        "Live",
        &crate::remote_log::tests::url(
            "(index:'*:log-2',query:(language:kuery,query:''),sort:!(!('@timestamp',desc)))",
            "(time:(from:'2026-10-19T12:00:01Z',to:'2026-10-20T12:01:00Z'))",
        ),
    )
    .unwrap();
    source.discover.time.to = "2026-10-20T12:00:01Z".into();
    source
}
fn serve(count: usize) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint =
        url::Url::parse(&format!("http://{}/s/test", listener.local_addr().unwrap())).unwrap();
    let state = Arc::new(Mutex::new(State {
        visible: (0..count).map(hit).collect(),
        ..Default::default()
    }));
    let stop = Arc::new(AtomicBool::new(false));
    let slow_started = Arc::new(AtomicBool::new(false));
    let disconnected = Arc::new(AtomicBool::new(false));
    let (st, done, slow, gone) = (
        state.clone(),
        stop.clone(),
        slow_started.clone(),
        disconnected.clone(),
    );
    let thread = thread::spawn(move || {
        let mut children = Vec::new();
        while !done.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let (st, slow, gone) = (st.clone(), slow.clone(), gone.clone());
                    children.push(thread::spawn(move || respond(stream, st, slow, gone)));
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(1))
                }
                Err(e) => panic!("listener: {e}"),
            }
        }
        for child in children {
            child.join().unwrap();
        }
    });
    Server {
        endpoint,
        state,
        stop,
        slow_started,
        disconnected,
        thread: Some(thread),
    }
}
fn respond(
    mut stream: TcpStream,
    state: Arc<Mutex<State>>,
    slow: Arc<AtomicBool>,
    gone: Arc<AtomicBool>,
) {
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut first = String::new();
    if reader.read_line(&mut first).is_err() || first.is_empty() {
        return;
    }
    let mut size = 0;
    let mut auth = false;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line.is_empty() {
            return;
        }
        if line == "\r\n" {
            break;
        }
        if line.to_ascii_lowercase().starts_with("content-length:") {
            size = line.split(':').nth(1).unwrap().trim().parse().unwrap();
        }
        if line.to_ascii_lowercase().starts_with("authorization:") {
            auth = line.contains("ApiKey test-secret");
        }
    }
    assert!(
        auth,
        "positive control: real authorized HTTP reached scripted server"
    );
    let mut bytes = vec![0; size];
    if reader.read_exact(&mut bytes).is_err() {
        return;
    }
    let body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    let target = first.split_whitespace().nth(1).unwrap();
    let mut status = 200;
    let response = {
        let mut s = state.lock().unwrap();
        if target.contains("/api/status") {
            json!({"version":{"number":"9.5.4"}})
        } else if target.contains("/api/data_views/") {
            json!({"data_view":{"title":"*:log-2","timeFieldName":"@timestamp"}})
        } else if target.contains("/api/console/proxy") && target.contains("method=POST") {
            s.opens += 1;
            let id = format!("pit-{}", s.opens);
            let mut snapshot = s.visible.clone();
            // Independent server oracle sorts the frozen snapshot; it neither
            // calls production query/paging code nor inserts late events into an old PIT.
            snapshot.sort_by(|a, b| {
                b["sort"][0]
                    .as_str()
                    .cmp(&a["sort"][0].as_str())
                    .then_with(|| a["sort"][1].as_u64().cmp(&b["sort"][1].as_u64()))
            });
            s.pits.insert(id.clone(), snapshot);
            let mut v = quality();
            v["id"] = json!(id);
            v
        } else if target.contains("/api/console/proxy") && target.contains("method=DELETE") {
            if s.fail_close {
                s.fail_close = false;
                status = 503;
                json!({"error":"offline during cleanup"})
            } else {
                let id = body["id"]
                    .as_str()
                    .expect("latest scalar PIT ID")
                    .to_owned();
                assert!(
                    s.pits.remove(&id).is_some(),
                    "cleanup must use latest rotated handle"
                );
                s.closes.push(id);
                json!({"succeeded":true,"num_freed":1})
            }
        } else if target == "/s/test/internal/search/es" {
            assert!(body["params"].get("index").is_none());
            assert_eq!(
                body["params"]["body"]["sort"][0]["@timestamp"]["order"],
                "desc"
            );
            assert!(body["params"]["body"]["query"]["bool"]["filter"]
                .as_array()
                .unwrap()
                .iter()
                .all(|v| v.get("@tsm.es_ts").is_none()));
            s.searches += 1;
            s.active += 1;
            s.peak = s.peak.max(s.active);
            if s.slow_next {
                s.slow_next = false;
                drop(s);
                slow.store(true, Ordering::Release);
                let mut one = [0];
                let n = reader
                    .read(&mut one)
                    .expect("cancel must close socket before deadline");
                assert_eq!(n, 0, "cancelled request must disconnect");
                state.lock().unwrap().active -= 1;
                gone.store(true, Ordering::Release);
                return;
            }
            if s.fail_next {
                s.fail_next = false;
                status = 503;
                json!({"error":"offline"})
            } else {
                let id = body["params"]["pit"]["id"].as_str().unwrap();
                let snapshot = s.pits.remove(id).expect("search must use current handle");
                let after = &body["params"]["body"]["search_after"];
                let start = if after.is_null() {
                    0
                } else {
                    s.cursors.push(after.clone());
                    snapshot
                        .iter()
                        .position(|h| &h["sort"] == after)
                        .expect("cursor JSON must be exact")
                        + 1
                };
                let size = body["params"]["body"]["size"].as_u64().unwrap() as usize;
                let hits = snapshot
                    .iter()
                    .skip(start)
                    .take(size)
                    .cloned()
                    .collect::<Vec<_>>();
                if s.late_on_pit == Some(s.opens) && start == 0 {
                    let mut late = hit(99_999);
                    late["_id"] = json!("hour-late");
                    late["_source"]["@timestamp"] = json!("2026-10-20T10:59:00.123456789Z");
                    late["sort"][0] = late["_source"]["@timestamp"].clone();
                    s.visible.push(late);
                    s.late_on_pit = None;
                }
                let next = format!("{id}-r");
                s.pits.insert(next.clone(), snapshot);
                let mut raw = quality();
                raw["pit_id"] = json!(next);
                raw["timed_out"] = json!(false);
                raw["hits"] = json!({"hits":hits});
                if s.partial_next {
                    s.partial_next = false;
                    raw["_clusters"]["skipped"] = json!(1);
                }
                json!({"isRunning":false,"isPartial":false,"rawResponse":raw})
            }
        } else {
            panic!("unexpected request {first}")
        }
    };
    if target == "/s/test/internal/search/es" {
        let delay = state.lock().unwrap().delay;
        thread::sleep(delay);
        state.lock().unwrap().active -= 1;
    }
    let data = serde_json::to_vec(&response).unwrap();
    let _=write!(stream,"HTTP/1.1 {status} OK\r\nConnection: close\r\nContent-Type: application/json\r\nx-console-proxy-status-code: 200\r\nContent-Length: {}\r\n\r\n",data.len());
    let _ = stream.write_all(&data);
}
fn worker(server: &Server) -> (Worker, async_channel::Receiver<Update>) {
    Worker::spawn(
        Timing {
            poll: Duration::from_millis(50),
            retry: Duration::from_millis(100),
            cycle: Duration::from_secs(10),
            idle: Duration::from_secs(45),
        },
        Some(server.endpoint.clone()),
    )
}
fn until(
    receiver: &async_channel::Receiver<Update>,
    predicate: impl Fn(&Update) -> bool,
) -> Update {
    let limit = Instant::now() + Duration::from_secs(10);
    loop {
        match receiver.try_recv() {
            Ok(value) if predicate(&value) => return value,
            Ok(_) => {}
            Err(async_channel::TryRecvError::Empty) => thread::sleep(Duration::from_millis(1)),
            Err(e) => panic!("worker closed: {e}"),
        }
        assert!(Instant::now() < limit, "expected live state never arrived");
    }
}
fn wait(predicate: impl Fn() -> bool) {
    let limit = Instant::now() + Duration::from_secs(10);
    while !predicate() {
        assert!(Instant::now() < limit);
        thread::sleep(Duration::from_millis(1));
    }
}
fn applied(worker: &Worker, receiver: &async_channel::Receiver<Update>) -> Arc<Loaded> {
    let generation = worker.apply(source(), ApiKey::new(b"test-secret".to_vec()).unwrap());
    until(receiver, |u| {
        u.generation == generation && matches!(u.status, Status::History { .. })
    })
    .loaded
    .unwrap()
}
fn start(worker: &Worker, loaded: &Arc<Loaded>) -> u64 {
    worker.start_live(Mode::Fixed(loaded.range.clone()), &loaded.source)
}
fn close(worker: Worker, receiver: &async_channel::Receiver<Update>, server: &Server) {
    drop(worker);
    wait(|| receiver.is_closed());
    assert!(server.state.lock().unwrap().pits.is_empty());
}

#[test]
fn full_window_cycles_find_hour_late_events_after_old_cursor_with_equal_nanos() {
    let server = serve(501);
    let (worker, events) = worker(&server);
    let initial = applied(&worker, &events);
    assert_eq!(initial.window.events.len(), PAGE_ROWS);
    server.state.lock().unwrap().late_on_pit = Some(2);
    let generation = start(&worker, &initial);
    let first = until(&events, |u| {
        u.generation == generation && matches!(u.status, Status::Complete { .. })
    });
    assert_eq!(first.loaded.as_ref().unwrap().window.events.len(), 501);
    assert!(
        !first
            .loaded
            .unwrap()
            .window
            .events
            .iter()
            .any(|e| e["_id"] == "hour-late"),
        "new visibility cannot alter an existing frozen PIT"
    );
    let second = until(&events, |u| {
        matches!(u.status, Status::Complete { .. })
            && u.loaded
                .as_ref()
                .unwrap()
                .window
                .events
                .iter()
                .any(|e| e["_id"] == "hour-late")
    });
    let data = &second.loaded.unwrap().window;
    assert_eq!(data.events.len(), 502);
    let identities: std::collections::HashSet<_> = data
        .events
        .iter()
        .map(|e| {
            (
                e["_index"].clone().to_string(),
                e["_id"].clone().to_string(),
            )
        })
        .collect();
    assert_eq!(identities.len(), 502);
    assert_eq!(data.events.last().unwrap()["_id"], "hour-late");
    assert_eq!(data.events[0]["sort"][0], data.events[1]["sort"][0]);
    assert!(data
        .events
        .windows(2)
        .all(|p| p[0]["sort"][0].as_str() >= p[1]["sort"][0].as_str()));
    assert!(server
        .state
        .lock()
        .unwrap()
        .cursors
        .iter()
        .any(|c| c[1].as_u64().unwrap() > 9_000_000_000_000_000_000));
    close(worker, &events, &server);
}
#[test]
fn reconnect_catches_up_several_pages_and_partial_ccs_is_incomplete_not_success() {
    let server = serve(201);
    let (worker, events) = worker(&server);
    let previous = applied(&worker, &events);
    {
        let mut state = server.state.lock().unwrap();
        state.visible = (0..601).map(hit).collect();
        state.fail_next = true;
        state.fail_close = true;
    }
    start(&worker, &previous);
    let offline = until(&events, |u| matches!(u.status, Status::Reconnecting { .. }));
    assert!(Arc::ptr_eq(
        &offline.loaded.unwrap().window,
        &previous.window
    ));
    assert_eq!(
        server.state.lock().unwrap().pits.len(),
        1,
        "outage retains the known handle until cleanup succeeds"
    );
    let recovered = until(&events, |u| {
        matches!(u.status, Status::Complete { pages: 4, .. })
    });
    let complete = recovered.loaded.unwrap();
    assert_eq!(complete.window.events.len(), 601);
    server.state.lock().unwrap().partial_next = true;
    let partial = until(&events, |u| matches!(u.status, Status::Incomplete { .. }));
    assert!(!partial.following);
    assert!(Arc::ptr_eq(
        &partial.loaded.unwrap().window,
        &complete.window
    ));
    assert!(server.state.lock().unwrap().pits.is_empty());
    close(worker, &events, &server);
}
#[test]
fn apply_and_close_abort_slow_search_without_publishing_stale_generation() {
    let server = serve(201);
    let (worker, events) = worker(&server);
    let initial = applied(&worker, &events);
    server.state.lock().unwrap().slow_next = true;
    start(&worker, &initial);
    wait(|| server.slow_started.load(Ordering::Acquire));
    let mut next = source();
    next.discover.query.query = "service:new".into();
    let started = Instant::now();
    let generation = worker.apply(next, ApiKey::new(b"test-secret".to_vec()).unwrap());
    let replaced = until(&events, |u| {
        u.generation == generation && matches!(u.status, Status::History { .. })
    });
    let replacement = replaced.loaded.unwrap();
    assert_eq!(replacement.source.discover.query.query, "service:new");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "Apply aborts socket; it does not wait the ten-second request deadline"
    );
    assert!(server.disconnected.load(Ordering::Acquire));
    server.slow_started.store(false, Ordering::Release);
    server.disconnected.store(false, Ordering::Release);
    server.state.lock().unwrap().slow_next = true;
    start(&worker, &replacement);
    wait(|| server.slow_started.load(Ordering::Acquire));
    close(worker, &events, &server);
    assert!(server.disconnected.load(Ordering::Acquire));
}
#[test]
fn background_consumer_is_not_needed_for_polling_stop_or_bounded_cleanup() {
    let server = serve(2);
    server.state.lock().unwrap().delay = Duration::from_millis(100);
    let (worker, events) = worker(&server);
    let initial = applied(&worker, &events);
    thread::sleep(Duration::from_millis(150));
    assert_eq!(
        server.state.lock().unwrap().opens,
        1,
        "imported pause never starts a cycle automatically"
    );
    start(&worker, &initial);
    // No event consumption simulates a background/suspended view. The channel
    // remains one coherent latest state, and polling never waits for rendering.
    wait(|| server.state.lock().unwrap().opens >= 4);
    assert!(events.len() <= 1);
    assert_eq!(
        server.state.lock().unwrap().peak,
        1,
        "positive control: several slow real polling cycles never overlap"
    );
    let generation = worker.stop();
    until(&events, |u| {
        u.generation == generation && matches!(u.status, Status::Stopped { .. })
    });
    assert!(server.state.lock().unwrap().pits.is_empty());
    close(worker, &events, &server);
}
#[test]
fn full_window_bound_never_labels_truncated_older_pages_complete_or_discards_previous() {
    let server = serve(MAX_ROWS + 1);
    let (worker, events) = worker(&server);
    let previous = applied(&worker, &events);
    start(&worker, &previous);
    let incomplete = until(&events, |u| {
        matches!(
            u.status,
            Status::Incomplete {
                scanned: MAX_ROWS,
                ..
            }
        )
    });
    assert!(!incomplete.following);
    assert!(Arc::ptr_eq(
        &incomplete.loaded.unwrap().window,
        &previous.window
    ));
    assert!(server.state.lock().unwrap().pits.is_empty());
    close(worker, &events, &server);
}
#[test]
fn rolling_proposal_keeps_duration_with_nanos_and_does_not_mutate_fixed_input() {
    let fixed = source().discover.time;
    let mode = Mode::rolling_same_duration(&fixed).unwrap();
    let now = OffsetDateTime::parse(
        "2026-10-21T10:00:00.123456789Z",
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    let range = mode.range(now).unwrap();
    assert_eq!(range.to, "2026-10-21T10:00:00.123456789Z");
    assert_eq!(range.from, "2026-10-20T10:00:00.123456789Z");
    assert_eq!(fixed.from, "2026-10-19T12:00:01Z");
}
