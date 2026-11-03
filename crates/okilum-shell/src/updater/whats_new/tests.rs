use super::*;
use std::net::TcpListener;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
const TRACK: Track = Track {
    platform: "macos",
    channel: "beta",
};

fn build(number: u64, source: char) -> Build {
    Build {
        build: number,
        source: source.to_string().repeat(40),
    }
}
fn fixture(source: &str) -> Result<Option<Vec<u8>>> {
    Ok(Some(match source.as_bytes()[0] {
        b'a' => include_bytes!("fixtures/15053.json").to_vec(),
        b'b' => include_bytes!("fixtures/15032.json").to_vec(),
        b'c' => include_bytes!("fixtures/15001.json").to_vec(),
        _ => anyhow::bail!("Offline"),
    }))
}

#[test]
fn first_install_is_silent_update_once_and_downgrade_keeps_high_water_mark() {
    let directory = tempfile::tempdir().unwrap();
    assert_eq!(
        record_launch(directory.path(), &build(15001, 'c')).unwrap(),
        None
    );
    assert_eq!(
        record_launch(directory.path(), &build(15053, 'a')).unwrap(),
        Some(15001)
    );
    assert_eq!(
        record_launch(directory.path(), &build(15053, 'a')).unwrap(),
        None
    );
    assert_eq!(
        record_launch(directory.path(), &build(15032, 'b')).unwrap(),
        None
    );
    assert_eq!(
        record_launch(directory.path(), &build(15053, 'a')).unwrap(),
        None
    );
    assert_eq!(
        record_launch(directory.path(), &build(15054, 'd')).unwrap(),
        Some(15053)
    );
}

#[test]
fn failed_or_busy_bookkeeping_never_announces_or_overwrites_the_state() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("whats-new.json");
    fs::write(&path, b"corrupt").unwrap();
    assert!(record_launch(directory.path(), &build(15053, 'a')).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"corrupt");
    fs::remove_file(&path).unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.path().join("whats-new.lock"))
        .unwrap();
    lock.lock().unwrap();
    assert!(record_launch(directory.path(), &build(15053, 'a')).is_err());
    assert!(!path.exists());
    drop(lock);
    assert_eq!(
        record_launch(directory.path(), &build(15053, 'a')).unwrap(),
        None
    );
}

#[test]
fn update_range_is_cumulative_newest_first_and_about_only_fetches_current() {
    let current = build(15053, 'a');
    let range = collect(&current, Some(15001), TRACK, fixture);
    assert!(range.unavailable.is_none());
    assert_eq!(
        range.builds.iter().map(|n| n.build).collect::<Vec<_>>(),
        [15053, 15032]
    );
    assert_eq!(range.builds[0].items[0].pr, 1482);
    assert_eq!(
        range.builds[0].items[0].link(),
        "https://git.oklabs.uk/BeFeast/okilum/pulls/1482"
    );
    assert_eq!(
        collect(&current, Some(15032), TRACK, fixture).builds.len(),
        1
    );
    assert_eq!(collect(&current, None, TRACK, fixture).builds.len(), 1);
    let earliest = collect(&build(15001, 'c'), Some(1), TRACK, fixture);
    assert!(earliest.unavailable.is_none());
    assert!(earliest.builds[0].items.is_empty());
}

#[test]
fn offline_and_partial_failure_keep_the_current_build_and_safe_links() {
    let current = build(15053, 'a');
    let offline = collect(&current, Some(15001), TRACK, |_| anyhow::bail!("Offline"));
    assert_eq!(offline.unavailable, Some(Unavailable::FetchFailed));
    assert!(offline.builds.is_empty());
    assert!(current.link().ends_with(&"a".repeat(40)));
    // Positive control: the exact same first fixture succeeds before the gap.
    let partial = collect(&current, Some(15001), TRACK, |sha| {
        if sha == current.source {
            fixture(sha)
        } else {
            anyhow::bail!("Offline")
        }
    });
    assert_eq!(partial.unavailable, Some(Unavailable::FetchFailed));
    assert_eq!(partial.builds.len(), 1);
    assert_eq!(partial.builds[0].items[0].pr, 1482);
}

#[test]
fn malformed_mismatched_and_oversized_notes_are_bounded_fallbacks() {
    let current = build(15053, 'a');
    for invalid in [
        b"not json".to_vec(),
        fixture(&"b".repeat(40)).unwrap().unwrap(),
        vec![b' '; MAX_BYTES as usize + 1],
    ] {
        let range = collect(&current, None, TRACK, |_| Ok(Some(invalid.clone())));
        assert_eq!(range.unavailable, Some(Unavailable::FetchFailed));
        assert!(range.builds.is_empty());
    }
    let mut cycle: serde_json::Value =
        serde_json::from_slice(&fixture(&current.source).unwrap().unwrap()).unwrap();
    cycle["previous_build"] = 15054.into();
    cycle["previous_source"] = "../not-a-source".into();
    let mut calls = 0;
    let range = collect(&current, Some(1), TRACK, |_| {
        calls += 1;
        Ok(Some(serde_json::to_vec(&cycle).unwrap()))
    });
    assert_eq!(calls, 1);
    assert_eq!(range.unavailable, Some(Unavailable::FetchFailed));
    assert!(read_bounded(&vec![0; 4097][..], 4096).is_err());
}

#[test]
fn notes_from_another_platform_or_channel_are_not_presented() {
    let current = build(15053, 'a');
    for track in [
        Track {
            platform: "windows",
            channel: "beta",
        },
        Track {
            platform: "macos",
            channel: "stable",
        },
    ] {
        let range = collect(&current, None, track, fixture);
        assert_eq!(range.unavailable, Some(Unavailable::FetchFailed));
        assert!(range.builds.is_empty());
    }
}

#[test]
fn half_a_predecessor_pair_keeps_current_notes_but_marks_history_incomplete() {
    let current = build(15053, 'a');
    for missing in ["previous_source", "previous_build"] {
        let mut notes: serde_json::Value =
            serde_json::from_slice(&fixture(&current.source).unwrap().unwrap()).unwrap();
        notes[missing] = serde_json::Value::Null;
        let range = collect(&current, Some(15001), TRACK, |_| {
            Ok(Some(serde_json::to_vec(&notes).unwrap()))
        });
        assert_eq!(range.unavailable, Some(Unavailable::FetchFailed));
        assert_eq!(range.builds.len(), 1);
        assert_eq!(range.builds[0].items[0].pr, 1482);
    }
    // Positive end-of-history control: both absent is a legitimate first release.
    let earliest = collect(&build(15001, 'c'), Some(1), TRACK, fixture);
    assert!(earliest.unavailable.is_none());
    assert_eq!(earliest.builds.len(), 1);
}

#[test]
fn unpublished_current_or_predecessor_keeps_its_distinct_status_and_available_items() {
    let current = build(15053, 'a');
    let unpublished = collect(&current, None, TRACK, |_| Ok(None));
    assert_eq!(unpublished.unavailable, Some(Unavailable::NotPublished));
    assert!(unpublished.builds.is_empty());
    let partial = collect(&current, Some(15001), TRACK, |sha| {
        if sha == current.source {
            fixture(sha)
        } else {
            Ok(None)
        }
    });
    assert_eq!(partial.unavailable, Some(Unavailable::NotPublished));
    assert_eq!(partial.builds.len(), 1);
    assert_eq!(partial.builds[0].items[0].pr, 1482);
    // Same chain, fully published: the fallback disappears.
    assert!(collect(&current, Some(15001), TRACK, fixture)
        .unavailable
        .is_none());
}

#[test]
fn http_404_is_unpublished_but_server_errors_stay_retryable() {
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/notes.json", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        for status in [200, 404, 503] {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = [0; 2048];
            assert!(socket.read(&mut request).unwrap() > 0);
            write!(
                socket,
                "HTTP/1.1 {status} Status\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
            )
            .unwrap();
        }
    });
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .build()
        .unwrap();
    // Positive transport control: this mock really serves a readable body.
    assert_eq!(
        fetch_notes(&client, &url, Duration::from_secs(3)).unwrap(),
        Some(b"{}".to_vec())
    );
    assert_eq!(
        fetch_notes(&client, &url, Duration::from_secs(3)).unwrap(),
        None
    );
    assert!(fetch_notes(&client, &url, Duration::from_secs(3)).is_err());
    server.join().unwrap();
}

#[test]
fn appimage_chain_uses_embedded_builds_and_rejects_publication_scale() {
    let track = Track {
        platform: "linux-appimage",
        channel: "beta",
    };
    let current = build(6955, 'a');
    let fetch = |source: &str| {
        let (number, previous_build, previous_source) = if source == "a".repeat(40) {
            (6955, Some(6950), Some("b".repeat(40)))
        } else {
            (6950, Some(6931), Some("c".repeat(40)))
        };
        Ok(Some(
            serde_json::to_vec(&serde_json::json!({
                "platform": "linux-appimage", "channel": "beta", "build": number,
                "source": source, "previous_build": previous_build,
                "previous_source": previous_source, "items": []
            }))
            .unwrap(),
        ))
    };
    let range = collect(&current, Some(6931), track, fetch);
    assert!(range.unavailable.is_none());
    assert_eq!(
        range.builds.iter().map(|n| n.build).collect::<Vec<_>>(),
        [6955, 6950]
    );
    let wrong_scale = collect(&current, None, track, |source| {
        let mut value: serde_json::Value = serde_json::from_slice(&fetch(source)?.unwrap())?;
        value["build"] = serde_json::json!(18581);
        Ok(Some(serde_json::to_vec(&value)?))
    });
    assert_eq!(wrong_scale.unavailable, Some(Unavailable::FetchFailed));
    assert!(wrong_scale.builds.is_empty());
}

/// One scripted reply per accepted connection. `Connection: close` gives every
/// notes request its own connection, so counts are exact (#2451).
struct Reply {
    status: u16,
    body: Vec<u8>,
    delay: Duration,
    hang: bool,
}
impl Reply {
    fn notes(body: &[u8]) -> Self {
        Reply {
            status: 200,
            body: body.to_vec(),
            delay: Duration::ZERO,
            hang: false,
        }
    }
    fn error(status: u16) -> Self {
        Reply {
            status,
            body: Vec::new(),
            delay: Duration::ZERO,
            hang: false,
        }
    }
    fn delayed(delay: Duration, body: &[u8]) -> Self {
        Reply {
            status: 200,
            body: body.to_vec(),
            delay,
            hang: false,
        }
    }
    fn hang() -> Self {
        Reply {
            status: 0,
            body: Vec::new(),
            delay: Duration::from_secs(600),
            hang: true,
        }
    }
}

/// A local HTTP responder: connection N is answered by script entry N and the
/// last entry repeats. No real network and no real feeds are involved (#2451).
struct FakeServer {
    url: String,
    requests: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}
impl FakeServer {
    fn start(script: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let handle = std::thread::spawn({
            let requests = requests.clone();
            let stop = stop.clone();
            move || serve(listener, &script, &requests, &stop)
        });
        Self {
            url,
            requests,
            stop,
            handle: Some(handle),
        }
    }
    fn served(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
    /// An accept can lag behind the client that already timed out on a hung
    /// reply, so wait for the expected count instead of reading it once.
    fn wait_served(&self, expected: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.served() < expected {
            assert!(
                Instant::now() < deadline,
                "only {} of {expected} expected requests arrived",
                self.served()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for FakeServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.handle.take().unwrap().join().unwrap();
    }
}
fn serve(listener: &TcpListener, script: &[Reply], requests: &AtomicUsize, stop: &AtomicBool) {
    while !stop.load(Ordering::SeqCst) {
        let Ok((mut socket, _)) = listener.accept() else {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        };
        let index = requests.fetch_add(1, Ordering::SeqCst);
        let Some(reply) = script.get(index).or_else(|| script.last()) else {
            continue;
        };
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut chunk = [0u8; 2048];
        let mut received = Vec::new();
        while !received.ends_with(b"\r\n\r\n") && received.len() < 2048 {
            match socket.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => received.extend_from_slice(&chunk[..read]),
            }
        }
        // Sleep in slices so drop() can end a long delay promptly.
        let mut waited = Duration::ZERO;
        while waited < reply.delay && !stop.load(Ordering::SeqCst) {
            let slice = Duration::from_millis(10).min(reply.delay - waited);
            std::thread::sleep(slice);
            waited += slice;
        }
        if reply.hang || stop.load(Ordering::SeqCst) {
            continue;
        }
        let reason = match reply.status {
            200 => "OK",
            404 => "Not Found",
            _ => "Unavailable",
        };
        let _ = write!(
            socket,
            "HTTP/1.1 {} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            reply.status,
            reply.body.len()
        );
        let _ = socket.write_all(&reply.body);
    }
}

fn test_profile(url: &str, attempts: usize) -> Profile {
    Profile {
        notes_root: url.to_owned(),
        first_connect: Duration::from_secs(2),
        request: Duration::from_millis(500),
        budget: Duration::from_secs(5),
        attempts,
    }
}
fn current_fixture() -> Vec<u8> {
    fixture(&"a".repeat(40)).unwrap().unwrap()
}
fn previous_fixture() -> Vec<u8> {
    fixture(&"b".repeat(40)).unwrap().unwrap()
}

#[test]
fn one_automatic_retry_shows_notes_after_a_transient_first_failure() {
    let current = build(15053, 'a');
    // Control: the identical blip without the automatic retry still fails,
    // which is the pre-#2451 behavior that hid notes that exist.
    let control = FakeServer::start(vec![Reply::error(503), Reply::notes(&current_fixture())]);
    let failed = load_on_track(&current, None, Some(TRACK), &test_profile(&control.url, 1));
    assert_eq!(failed.unavailable, Some(Unavailable::FetchFailed));
    assert!(failed.builds.is_empty());
    assert_eq!(control.served(), 1);
    // With one automatic retry the same server delivers this build's notes.
    let server = FakeServer::start(vec![Reply::error(503), Reply::notes(&current_fixture())]);
    let range = load_on_track(&current, None, Some(TRACK), &test_profile(&server.url, 2));
    assert_eq!(range.unavailable, None);
    assert_eq!(
        range.builds.iter().map(|n| n.build).collect::<Vec<_>>(),
        [15053]
    );
    assert_eq!(range.builds[0].items[0].pr, 1482);
    assert_eq!(
        server.served(),
        2,
        "exactly one retry after the first failure"
    );
}

#[test]
fn a_transient_mid_chain_failure_is_retried_into_the_full_gap() {
    let current = build(15053, 'a');
    let server = FakeServer::start(vec![
        Reply::notes(&current_fixture()),
        Reply::error(503),
        Reply::notes(&current_fixture()),
        Reply::notes(&previous_fixture()),
    ]);
    let range = load_on_track(
        &current,
        Some(15001),
        Some(TRACK),
        &test_profile(&server.url, 2),
    );
    assert_eq!(range.unavailable, None);
    assert_eq!(
        range.builds.iter().map(|n| n.build).collect::<Vec<_>>(),
        [15053, 15032]
    );
    assert_eq!(server.served(), 4, "the retry walks the gap chain again");
}

#[test]
fn a_persistent_mid_chain_failure_keeps_the_builds_that_did_load() {
    let current = build(15053, 'a');
    let server = FakeServer::start(vec![
        Reply::notes(&current_fixture()),
        Reply::error(503),
        Reply::notes(&current_fixture()),
        Reply::error(503),
    ]);
    let range = load_on_track(
        &current,
        Some(15001),
        Some(TRACK),
        &test_profile(&server.url, 2),
    );
    assert_eq!(range.unavailable, Some(Unavailable::FetchFailed));
    assert_eq!(
        range.builds.iter().map(|n| n.build).collect::<Vec<_>>(),
        [15053],
        "the newest build's notes stay visible instead of the error"
    );
    assert_eq!(range.builds[0].items[0].pr, 1482);
    assert_eq!(server.served(), 4);
    // A retry that regresses even further must not discard the first partial.
    let regressed = FakeServer::start(vec![
        Reply::notes(&current_fixture()),
        Reply::error(503),
        Reply::error(503),
    ]);
    let range = load_on_track(
        &current,
        Some(15001),
        Some(TRACK),
        &test_profile(&regressed.url, 2),
    );
    assert_eq!(range.unavailable, Some(Unavailable::FetchFailed));
    assert_eq!(
        range.builds.iter().map(|n| n.build).collect::<Vec<_>>(),
        [15053]
    );
    assert_eq!(regressed.served(), 3);
}

#[test]
fn a_hanging_server_stays_bounded_and_retries_exactly_once() {
    let current = build(15053, 'a');
    let server = FakeServer::start(vec![Reply::hang()]);
    let mut profile = test_profile(&server.url, 2);
    profile.first_connect = Duration::from_millis(200);
    profile.request = Duration::from_millis(200);
    profile.budget = Duration::from_millis(400);
    let started = Instant::now();
    let range = load_on_track(&current, None, Some(TRACK), &profile);
    let elapsed = started.elapsed();
    assert_eq!(range.unavailable, Some(Unavailable::FetchFailed));
    assert!(range.builds.is_empty());
    server.wait_served(2);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(
        server.served(),
        2,
        "one initial attempt plus one retry, no loop"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "attempts × budget bounds the total, took {elapsed:?}"
    );
}

#[test]
fn an_unknown_channel_never_fetches_and_says_what_is_wrong() {
    let current = build(15053, 'a');
    // The server would happily serve notes; the channel alone is the problem.
    let server = FakeServer::start(vec![Reply::notes(&current_fixture())]);
    let range = load_on_track(&current, Some(15001), None, &test_profile(&server.url, 2));
    assert_eq!(range.unavailable, Some(Unavailable::UnknownChannel));
    assert!(range.builds.is_empty());
    assert_eq!(
        server.served(),
        0,
        "a deterministic problem makes no pointless request or retry"
    );
}

#[test]
fn the_first_request_gets_a_longer_cap_than_later_requests() {
    let current = build(15053, 'a');
    let slow = Duration::from_millis(350);
    let server = FakeServer::start(vec![
        Reply::delayed(slow, &current_fixture()),
        Reply::delayed(slow, &previous_fixture()),
    ]);
    let mut profile = test_profile(&server.url, 1);
    profile.first_connect = Duration::from_secs(2);
    profile.request = Duration::from_millis(100);
    let range = load_on_track(&current, Some(15001), Some(TRACK), &profile);
    // The first, slow fetch fits under the longer first-connect cap...
    assert_eq!(
        range.builds.iter().map(|n| n.build).collect::<Vec<_>>(),
        [15053]
    );
    // ...while the second slow fetch is cut by the ordinary per-request cap.
    assert_eq!(range.unavailable, Some(Unavailable::FetchFailed));
    server.wait_served(2);
    assert_eq!(
        server.served(),
        2,
        "no further requests after the cut-off second fetch"
    );
}

#[test]
fn production_retries_once_with_a_longer_first_connect_inside_a_bounded_total() {
    let profile = production_profile();
    assert_eq!(profile.attempts, 2, "exactly one automatic retry");
    assert!(
        profile.first_connect > profile.request,
        "the first request gets the longer cap"
    );
    assert!(
        profile.first_connect <= profile.budget,
        "the first request fits inside one attempt's budget"
    );
    assert_eq!(
        profile.budget * profile.attempts,
        Duration::from_secs(20),
        "the documented total bound"
    );
}
