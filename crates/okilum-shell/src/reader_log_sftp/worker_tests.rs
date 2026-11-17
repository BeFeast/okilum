use super::*;
use ::core::prelude::v1::test;
use std::sync::Mutex;

struct Fake {
    bytes: Arc<Mutex<Vec<u8>>>,
    reads: Arc<Mutex<Vec<u64>>>,
    cancel_on_read: Option<Arc<AtomicBool>>,
}
impl Transport for Fake {
    fn open(&mut self, _: &Target) -> io::Result<()> {
        Ok(())
    }
    fn read_at(&mut self, offset: u64, limit: usize) -> io::Result<Vec<u8>> {
        self.reads.lock().unwrap().push(offset);
        if let Some(cancel) = &self.cancel_on_read {
            cancel.store(true, Ordering::Release);
        }
        let bytes = self.bytes.lock().unwrap();
        let start = (offset as usize).min(bytes.len());
        Ok(bytes[start..bytes.len().min(start + limit)].to_vec())
    }
    fn disconnect(&mut self) {}
}
fn fixture(bytes: &[u8]) -> Fake {
    Fake {
        bytes: Arc::new(Mutex::new(bytes.to_vec())),
        reads: Arc::new(Mutex::new(Vec::new())),
        cancel_on_read: None,
    }
}
#[test]
fn snapshot_reads_exact_lines_and_reports_unfinished_tail() {
    let fake = fixture("one\r\n雪\npartial".as_bytes());
    let mut frames = Vec::new();
    run(
        Target::parse("sftp://fixture/var/app.log").unwrap(),
        fake,
        false,
        "snapshot".into(),
        Arc::new(AtomicBool::new(false)),
        |frame| {
            frames.push(frame);
            true
        },
    );
    let last = frames.last().unwrap();
    assert!(!last.active && !last.error);
    assert_eq!(
        last.data.as_ref().unwrap().bytes(),
        "one\r\n雪\n".as_bytes()
    );
    assert!(last.message.contains("7 trailing bytes"));
}
#[test]
fn rewrite_stops_follow_without_replacing_loaded_rows() {
    let fake = fixture(b"old\n");
    let bytes = fake.bytes.clone();
    let mut last = None;
    run(
        Target::parse("sftp://fixture/app.log").unwrap(),
        fake,
        true,
        "rewrite".into(),
        Arc::new(AtomicBool::new(false)),
        |frame| {
            if frame.active && frame.data.is_some() {
                *bytes.lock().unwrap() = b"new\n".to_vec();
            }
            last = Some(frame);
            true
        },
    );
    let last = last.unwrap();
    assert!(!last.active && last.error);
    assert!(last.message.contains("continuity lost"));
    assert_eq!(last.data.unwrap().bytes(), b"old\n");
}
#[test]
fn cancellation_between_prefix_reads_does_not_start_another_operation() {
    let cancel = Arc::new(AtomicBool::new(false));
    let mut fake = fixture(b"first\n");
    fake.cancel_on_read = Some(cancel.clone());
    let reads = fake.reads.clone();
    let mut last = None;
    run(
        Target::parse("sftp://fixture/app.log").unwrap(),
        fake,
        true,
        "cancel".into(),
        cancel,
        |frame| {
            last = Some(frame);
            true
        },
    );
    assert_eq!(*reads.lock().unwrap(), vec![0]);
    let last = last.unwrap();
    assert!(!last.active && !last.error);
    assert_eq!(last.message, "Stopped");
}

/// A remote file plus a scripted connection: each queued failure drops one
/// positional read, as a killed SSH connection does mid-poll.
#[derive(Default)]
struct Remote {
    bytes: Vec<u8>,
    drops: usize,
    opens: usize,
}
struct Flaky(Arc<Mutex<Remote>>);
impl Transport for Flaky {
    fn open(&mut self, _: &Target) -> io::Result<()> {
        self.0.lock().unwrap().opens += 1;
        Ok(())
    }
    fn read_at(&mut self, offset: u64, limit: usize) -> io::Result<Vec<u8>> {
        let mut remote = self.0.lock().unwrap();
        if remote.drops > 0 {
            remote.drops -= 1;
            return Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "synthetic connection drop",
            ));
        }
        let start = (offset as usize).min(remote.bytes.len());
        Ok(remote.bytes[start..remote.bytes.len().min(start + limit)].to_vec())
    }
    fn disconnect(&mut self) {}
}
fn follow_flaky(
    remote: Arc<Mutex<Remote>>,
    cancel: Arc<AtomicBool>,
    mut send: impl FnMut(&Frame),
    pause: impl FnMut(Duration),
) -> Vec<Frame> {
    let mut frames = Vec::new();
    run_paced(
        Target::parse("sftp://fixture/var/app.log").unwrap(),
        Flaky(remote),
        true,
        "reconnect".into(),
        cancel,
        |frame| {
            send(&frame);
            frames.push(frame);
            frames.len() < 200
        },
        pause,
    );
    frames
}

const DONE: &[u8] = b"one\ntwo\npartial\nthree\n";
#[test]
fn dropped_connection_reconnects_without_duplicate_or_missing_records() {
    let remote = Arc::new(Mutex::new(Remote {
        bytes: b"one\ntwo\npar".to_vec(),
        ..Remote::default()
    }));
    let cancel = Arc::new(AtomicBool::new(false));
    let (grow, stop) = (remote.clone(), cancel.clone());
    let mut ticks = 0;
    let frames = follow_flaky(
        remote.clone(),
        cancel.clone(),
        move |frame| {
            if frame.data.as_ref().is_some_and(|data| data.bytes() == DONE) {
                stop.store(true, Ordering::Release);
            }
        },
        move |_| {
            ticks += 1;
            if ticks == 1 {
                // The connection dies, and the file grows while it is down,
                // completing the partial record the session already holds.
                let mut remote = grow.lock().unwrap();
                remote.drops = 2;
                remote.bytes.extend_from_slice(b"tial\nthree\n");
            }
        },
    );
    let down: Vec<_> = frames.iter().filter(|f| f.active && f.error).collect();
    assert_eq!(down.len(), 2, "each failed reconnect is visible");
    assert!(down[0].message.contains("synthetic connection drop"));
    assert!(down[0]
        .message
        .contains("Reconnecting in 1s (retry 1 of 4)"));
    assert!(down[1]
        .message
        .contains("Reconnecting in 2s (retry 2 of 4)"));
    assert!(
        down.iter().all(|f| f.data.is_none()),
        "rows are not republished"
    );
    let last_down = frames.iter().rposition(|f| f.active && f.error).unwrap();
    assert!(
        frames[last_down + 1..]
            .iter()
            .any(|f| f.active && !f.error && f.data.as_ref().is_some_and(|d| d.bytes() == DONE)),
        "the view leaves the disconnected state with the recovered rows"
    );
    let last = frames.last().unwrap();
    assert!(!last.active && !last.error);
    assert_eq!(last.message, "Stopped");
    assert_eq!(last.data.as_ref().unwrap().bytes(), DONE);
    // Initial read, one EOF tick, two dropped polls, one recovered poll.
    assert_eq!(remote.lock().unwrap().opens, 5);
}

#[test]
fn five_consecutive_drops_stop_with_rows_retained_and_backoff_doubles() {
    let remote = Arc::new(Mutex::new(Remote {
        bytes: b"kept\n".to_vec(),
        ..Remote::default()
    }));
    let grow = remote.clone();
    let mut delays = Vec::new();
    let frames = follow_flaky(
        remote.clone(),
        Arc::new(AtomicBool::new(false)),
        |_| {},
        |delay| {
            delays.push(delay);
            if delays.len() == 1 {
                grow.lock().unwrap().drops = usize::MAX;
            }
        },
    );
    let secs: Vec<_> = delays.iter().map(Duration::as_secs).collect();
    // One follow tick, then the 1/2/4/8 s budget; the fifth failure stops.
    assert_eq!(secs, vec![1, 1, 2, 4, 8]);
    let last = frames.last().unwrap();
    assert!(!last.active && last.error);
    assert!(last.message.contains("failed 5 times in a row"));
    assert!(last.message.contains("Start a new session"));
    assert_eq!(last.data.as_ref().unwrap().bytes(), b"kept\n");
}

#[test]
fn successful_poll_resets_the_reconnect_budget() {
    let remote = Arc::new(Mutex::new(Remote {
        bytes: b"line\n".to_vec(),
        ..Remote::default()
    }));
    let cancel = Arc::new(AtomicBool::new(false));
    let (flaky, stop) = (remote.clone(), cancel.clone());
    let mut ticks = 0;
    let frames = follow_flaky(
        remote,
        cancel,
        |_| {},
        move |delay| {
            ticks += 1;
            // Follow ticks (1 s after EOF) alternate with outages of four
            // drops each; without a reset the second outage would exhaust it.
            match ticks {
                1 | 6 => flaky.lock().unwrap().drops = 4,
                11 => stop.store(true, Ordering::Release),
                _ => assert!(delay <= Duration::from_secs(8)),
            }
        },
    );
    let last = frames.last().unwrap();
    assert_eq!(last.message, "Stopped");
    assert!(!last.error);
    assert_eq!(frames.iter().filter(|f| f.active && f.error).count(), 8);
}

#[test]
fn failure_before_first_read_is_reported_without_retrying() {
    let remote = Arc::new(Mutex::new(Remote {
        bytes: b"never\n".to_vec(),
        drops: 1,
        ..Remote::default()
    }));
    let mut delays = 0;
    let frames = follow_flaky(
        remote.clone(),
        Arc::new(AtomicBool::new(false)),
        |_| {},
        |_| delays += 1,
    );
    assert_eq!(delays, 0);
    assert_eq!(frames.len(), 1);
    assert!(!frames[0].active && frames[0].error);
    assert!(frames[0].message.contains("Start a new session to retry"));
    assert_eq!(remote.lock().unwrap().opens, 1);
}

#[test]
fn stop_during_reconnect_backoff_opens_nothing_further() {
    let remote = Arc::new(Mutex::new(Remote {
        bytes: b"kept\n".to_vec(),
        ..Remote::default()
    }));
    let cancel = Arc::new(AtomicBool::new(false));
    let (flaky, stop) = (remote.clone(), cancel.clone());
    let mut ticks = 0;
    let frames = follow_flaky(
        remote.clone(),
        cancel,
        |_| {},
        move |_| {
            ticks += 1;
            match ticks {
                1 => flaky.lock().unwrap().drops = usize::MAX,
                _ => stop.store(true, Ordering::Release),
            }
        },
    );
    let last = frames.last().unwrap();
    assert!(!last.active && !last.error);
    assert_eq!(last.message, "Stopped");
    assert_eq!(last.data.as_ref().unwrap().bytes(), b"kept\n");
    // First read, EOF tick, one dropped poll; nothing after Stop.
    assert_eq!(remote.lock().unwrap().opens, 3);
}

#[test]
fn production_wait_returns_promptly_when_cancelled() {
    let start = std::time::Instant::now();
    wait(&AtomicBool::new(true), Duration::from_secs(8));
    assert!(start.elapsed() < Duration::from_secs(1));
}
