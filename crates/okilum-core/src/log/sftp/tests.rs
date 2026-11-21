use super::*;
use std::{cell::RefCell, rc::Rc};

#[derive(Default)]
struct Remote {
    data: Vec<u8>,
    fail_at: Option<u64>,
    opens: usize,
    reopens: usize,
    disconnects: usize,
    short: bool,
    /// Bytes returned by all reads, to show the prefix is not re-read.
    transferred: usize,
    report_size: bool,
}
struct Mock(Rc<RefCell<Remote>>);
impl Transport for Mock {
    fn open(&mut self, _: &Target) -> io::Result<()> {
        self.0.borrow_mut().opens += 1;
        Ok(())
    }
    fn read_at(&mut self, offset: u64, limit: usize) -> io::Result<Vec<u8>> {
        let mut remote = self.0.borrow_mut();
        if remote.fail_at == Some(offset) {
            remote.fail_at = None;
            return Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "synthetic dropped response",
            ));
        }
        let start = (offset as usize).min(remote.data.len());
        let size = if remote.short { limit.min(3) } else { limit };
        let bytes = remote.data[start..remote.data.len().min(start + size)].to_vec();
        remote.transferred += bytes.len();
        Ok(bytes)
    }
    fn reopen(&mut self, _: &Target) -> io::Result<()> {
        self.0.borrow_mut().reopens += 1;
        Ok(())
    }
    fn size(&mut self) -> io::Result<Option<u64>> {
        let remote = self.0.borrow();
        Ok(remote.report_size.then_some(remote.data.len() as u64))
    }
    fn disconnect(&mut self) {
        self.0.borrow_mut().disconnects += 1;
    }
}
fn setup(data: &[u8]) -> (Follow<Mock>, Rc<RefCell<Remote>>) {
    let remote = Rc::new(RefCell::new(Remote {
        data: data.into(),
        ..Remote::default()
    }));
    (
        Follow::new(
            Target::parse("sftp://homelab/var/log/app.log").unwrap(),
            Mock(remote.clone()),
        ),
        remote,
    )
}

#[test]
fn dropped_growth_response_retries_exact_cursor_without_duplicate_or_missing_records() {
    let (mut follow, remote) = setup(b"one\ntwo\n");
    assert_eq!(follow.poll(), Update::Records(b"one\ntwo\n".to_vec()));
    remote.borrow_mut().data.extend_from_slice(b"three\nfour\n");
    remote.borrow_mut().fail_at = Some(0); // the overlap read starts at 0
    assert!(matches!(follow.poll(), Update::Disconnected(_)));
    assert_eq!(follow.offset(), 8);
    assert_eq!(follow.poll(), Update::Records(b"three\nfour\n".to_vec()));
    assert_eq!(follow.poll(), Update::CaughtUp);
    // One connection until the drop, one after it; the EOF poll keeps it.
    assert_eq!(remote.borrow().opens, 2);
    assert_eq!(remote.borrow().disconnects, 1);
    assert_eq!(follow.poll(), Update::CaughtUp);
    assert_eq!(remote.borrow().reopens, 1, "EOF reopens the path, not ssh");
    assert_eq!(remote.borrow().opens, 2);
}

#[test]
fn partial_utf8_record_survives_disconnect_and_short_reads() {
    let record = "שלום🙂\n".as_bytes();
    let cut = record.len() - 3;
    let (mut follow, remote) = setup(&record[..cut]);
    assert_eq!(follow.poll(), Update::Pending);
    remote.borrow_mut().fail_at = Some(0); // the overlap read starts at 0
    assert!(matches!(follow.poll(), Update::Disconnected(_)));
    remote.borrow_mut().short = true;
    remote.borrow_mut().data.extend_from_slice(&record[cut..]);
    assert_eq!(follow.poll(), Update::Records(record.to_vec()));
    assert_eq!(follow.offset(), record.len() as u64);
}

#[test]
fn rotation_truncation_and_same_length_rewrite_stop_without_replaying_old_rows() {
    for replacement in [b"".as_slice(), b"new\n", b"new generation\n"] {
        let (mut follow, remote) = setup(b"old\n");
        assert_eq!(follow.poll(), Update::Records(b"old\n".to_vec()));
        remote.borrow_mut().data = replacement.to_vec();
        assert_eq!(follow.poll(), Update::ContinuityLost);
        assert_eq!(follow.offset(), 4);
        let opens = remote.borrow().opens;
        assert_eq!(follow.poll(), Update::ContinuityLost);
        assert_eq!(
            remote.borrow().opens,
            opens,
            "terminal ambiguity must not reconnect"
        );
    }
}

#[test]
fn rewrite_inside_the_proof_window_is_detected_on_reopen_and_at_the_cursor() {
    let mut data = vec![b'x'; CHUNK * 2 + 5];
    data.push(b'\n');
    // Reopen after EOF proves the last PROOF bytes, not only the cursor.
    let (mut follow, remote) = setup(&data);
    while follow.offset() < data.len() as u64 {
        follow.poll();
    }
    assert_eq!(follow.poll(), Update::CaughtUp);
    remote.borrow_mut().data[data.len() - PROOF + 1] = b'y';
    assert_eq!(follow.poll(), Update::ContinuityLost);
    // While attached, every growth read re-reads OVERLAP bytes before it.
    let (mut follow, remote) = setup(&data);
    while follow.offset() < data.len() as u64 {
        follow.poll();
    }
    let mut remote = remote.borrow_mut();
    remote.data[data.len() - OVERLAP] = b'y';
    remote.data.extend_from_slice(b"more\n");
    drop(remote);
    assert_eq!(follow.poll(), Update::ContinuityLost);
}

#[test]
fn copytruncate_while_attached_never_emits_the_new_generation_as_growth() {
    let (mut follow, remote) = setup(b"old line\n");
    assert_eq!(follow.poll(), Update::Records(b"old line\n".to_vec()));
    remote.borrow_mut().data = b"a new generation\n".to_vec();
    assert_eq!(follow.poll(), Update::ContinuityLost);
}

#[test]
fn a_followed_log_beyond_eight_mib_uses_one_connection_and_reads_each_byte_once() {
    let line = b"2026-11-20T10:00:00Z info request served in 12 ms\n";
    let data: Vec<u8> = line
        .iter()
        .copied()
        .cycle()
        .take(line.len() * 200_000)
        .collect();
    assert!(data.len() > 9 * 1024 * 1024);
    let (mut follow, remote) = setup(&data);
    let mut delivered = 0;
    loop {
        match follow.poll() {
            Update::Records(bytes) => delivered += bytes.len(),
            Update::CaughtUp => break,
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(delivered, data.len());
    let remote = remote.borrow();
    assert_eq!(
        (remote.opens, remote.reopens, remote.disconnects),
        (1, 0, 0)
    );
    // Each growth read repeats at most OVERLAP bytes; no prefix re-reads.
    let reads = data.len().div_ceil(CHUNK - OVERLAP) + 1;
    assert!(remote.transferred <= data.len() + reads * OVERLAP);
}

#[test]
fn tail_start_skips_the_cut_line_and_keeps_an_exact_boundary() {
    let data = b"first line\nsecond line\nthird\n";
    let both = b"second line\nthird\n".as_slice();
    // 6 and 18 begin exactly at a line start; 10 and 21 cut a line.
    for (tail, first) in [
        (6, b"third\n".as_slice()),
        (10, b"third\n"),
        (18, both),
        (21, both),
    ] {
        let (follow, remote) = setup(data);
        remote.borrow_mut().report_size = true;
        let mut follow = follow.tail(tail);
        let mut seen = Vec::new();
        loop {
            match follow.poll() {
                Update::Records(bytes) => seen.extend(bytes),
                Update::Pending => {}
                Update::CaughtUp => break,
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(seen, first, "tail {tail}");
        assert_eq!(follow.started_at(), (data.len() - tail as usize) as u64);
        // The view starts after the cut, and growth still follows.
        remote.borrow_mut().data.extend_from_slice(b"fourth\n");
        assert_eq!(follow.poll(), Update::Records(b"fourth\n".to_vec()));
    }
    // A smaller file, or a server without a size, is read from the start.
    let (follow, _remote) = setup(data);
    let mut follow = follow.tail(10);
    assert_eq!(follow.poll(), Update::Records(data.to_vec()));
    assert_eq!(follow.started_at(), 0);
}

#[test]
fn failed_prefix_verification_never_changes_partial_or_emitted_cursor() {
    let data = vec![b'x'; CHUNK + 5];
    let (mut follow, remote) = setup(&data);
    follow.poll();
    follow.poll();
    // Drop the connection, then fail the second proof read after reconnecting.
    remote.borrow_mut().fail_at = Some((data.len() - OVERLAP) as u64);
    assert!(matches!(follow.poll(), Update::Disconnected(_)));
    remote.borrow_mut().fail_at = Some(CHUNK as u64);
    assert!(matches!(follow.poll(), Update::Disconnected(_)));
    assert_eq!(remote.borrow().opens, 2);
    assert_eq!(follow.offset(), data.len() as u64);
    remote.borrow_mut().data.extend_from_slice(b"\nnext\n");
    let mut expected = data;
    expected.extend_from_slice(b"\nnext\n");
    assert_eq!(follow.poll(), Update::Records(expected));
}

#[test]
fn history_limit_is_explicit_and_no_more_network_reads_occur() {
    let (mut follow, remote) = setup(b"");
    follow.window = vec![b'x'; RECORD_LIMIT];
    follow.offset = RECORD_LIMIT as u64;
    assert_eq!(follow.poll(), Update::LimitReached);
    assert_eq!(remote.borrow().opens, 0);
    assert_eq!(follow.offset(), RECORD_LIMIT as u64);
}

#[test]
fn urls_are_not_shell_commands_and_never_include_credentials() {
    let target = Target::parse("sftp://home_alias/var/log/a%20b%3B%24%28id%29.log").unwrap();
    assert_eq!(target.path, b"/var/log/a b;$(id).log");
    let (command, askpass) = target.command(&Default::default());
    assert!(askpass.is_none());
    assert_eq!(command.get_program(), "ssh");
    let args = command
        .get_args()
        .map(|a| a.to_str().unwrap())
        .collect::<Vec<_>>();
    assert!(args
        .windows(4)
        .any(|a| a == ["-s", "--", "home_alias", "sftp"]));
    assert!(args.contains(&"-oBatchMode=yes"));
    assert!(args.contains(&"-oStrictHostKeyChecking=yes"));
    assert!(!args.iter().any(|a| a.contains("$(id)")));
    for url in [
        "sftp://user:password@host/x",
        "sftp://user@host/x",
        "sftp://-oProxyCommand=x/x",
        "sftp://host/x?password=x",
        "sftp://host/x#fragment",
        "sftp://host/%00",
        "sftp://host/%GG",
        "https://host/x",
        "sftp:///x",
        "sftp://host:22/x",
    ] {
        assert!(Target::parse(url).is_err(), "{url}");
    }
}

#[test]
fn markdown_targets_pass_user_and_port_as_separate_ssh_arguments() {
    let target = Target::parse_login("sftp://oleg@build-box:2222/srv/a.md").unwrap();
    assert!(target.interactive);
    let (command, _askpass) = target.command(&Default::default());
    let args = command
        .get_args()
        .map(|a| a.to_str().unwrap())
        .collect::<Vec<_>>();
    let tail = args.len() - 4;
    assert_eq!(args[tail..], ["-s", "--", "build-box", "sftp"]);
    assert!(args.windows(2).any(|a| a == ["-l", "oleg"]));
    assert!(args.windows(2).any(|a| a == ["-p", "2222"]));
    for url in [
        "sftp://user:pw@host/x",
        "sftp://-oProxyCommand=x@host/x",
        "sftp://user@host:99999/x",
        "sftp://user@host:-1/x",
    ] {
        assert!(Target::parse_login(url).is_err(), "{url}");
    }
    // Log following stays alias-only and non-interactive.
    assert!(
        !Target::parse("sftp://alias/var/log/a.log")
            .unwrap()
            .interactive
    );
}

#[cfg(unix)]
#[test]
fn installed_askpass_bridge_lets_only_markdown_targets_ask() {
    crate::ssh_askpass::install("/opt/okilum/okilum".into(), |request| request.answer(None))
        .unwrap();
    let clock = Arc::new(Clock::default());
    let (markdown, askpass) = Target::parse_login("sftp://alias/notes/a.md")
        .unwrap()
        .command(&clock);
    let askpass = askpass.expect("markdown targets register an askpass session");
    let args = markdown
        .get_args()
        .map(|a| a.to_str().unwrap())
        .collect::<Vec<_>>();
    assert!(args.contains(&"-oBatchMode=no"));
    assert!(args.contains(&"-oStrictHostKeyChecking=ask"));
    let env = markdown
        .get_envs()
        .map(|(k, v)| (k.to_str().unwrap(), v.map(|v| v.to_str().unwrap())))
        .collect::<Vec<_>>();
    assert!(env.contains(&("SSH_ASKPASS", Some("/opt/okilum/okilum"))));
    assert!(env.contains(&("SSH_ASKPASS_REQUIRE", Some("force"))));
    assert!(env
        .iter()
        .any(|(k, v)| *k == crate::ssh_askpass::SOCKET_ENV && v.is_some()));
    assert!(env.contains(&(crate::ssh_askpass::SESSION_ENV, Some(askpass.id()))));
    // Log following: no session, so neither prompts nor paused deadlines.
    let (log, session) = Target::parse("sftp://alias/var/log/a.log")
        .unwrap()
        .command(&clock);
    assert!(session.is_none());
    let args = log
        .get_args()
        .map(|a| a.to_str().unwrap())
        .collect::<Vec<_>>();
    assert!(args.contains(&"-oBatchMode=yes"));
    assert_eq!(log.get_envs().count(), 0);
}

#[cfg(unix)]
#[test]
fn cancel_aborts_a_waiting_request_and_kills_the_process() {
    // Stands in for ssh waiting on authentication: never answers.
    let mut child = Command::new("sleep")
        .arg("30")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    let input = child.stdin.take().unwrap();
    let (_sender, output) = mpsc::sync_channel(1);
    let cancel = Arc::new(AtomicBool::new(false));
    let clock = Arc::new(Clock::default());
    let askpass = crate::ssh_askpass::session_for_test(clock.clone());
    let mut session = Session {
        child,
        input,
        output,
        id: 0,
        handle: Vec::new(),
        clock: Some(clock),
        cancel: Some(cancel.clone()),
        _askpass: Some(askpass),
    };
    let stopper = {
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            cancel.store(true, Ordering::Release);
        })
    };
    let started = std::time::Instant::now();
    let error = session.exchange(&[1, 0, 0, 0, 3]).unwrap_err();
    stopper.join().unwrap();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "not the 15 s deadline"
    );
    drop(session);
    // The child is reaped: the pid no longer exists.
    let alive = std::path::Path::new(&format!("/proc/{pid}")).exists();
    assert!(!alive || !cfg!(target_os = "linux"));
}

#[test]
fn packet_framing_is_bounded_and_rejects_truncated_responses() {
    assert_eq!(
        packet(&mut [0, 0, 0, 2, 103, 7].as_slice()).unwrap(),
        [103, 7]
    );
    for bytes in [
        vec![0, 0, 0, 0],
        vec![0, 0, 0, 3, 103],
        (PACKET_LIMIT as u32 + 1).to_be_bytes().to_vec(),
    ] {
        assert!(packet(&mut bytes.as_slice()).is_err());
    }
    assert!(take_string(&mut [0, 0, 0, 8, 1].as_slice()).is_err());
}

#[test]
fn fstat_size_is_read_only_when_its_flag_is_set() {
    let mut attrs = 1u32.to_be_bytes().to_vec();
    attrs.extend_from_slice(&(9u64 << 32).to_be_bytes());
    assert_eq!(decode_size(105, &attrs).unwrap(), Some(9 << 32));
    assert_eq!(decode_size(105, &4u32.to_be_bytes()).unwrap(), None);
    assert!(decode_size(105, &1u32.to_be_bytes()).is_err());
    assert!(decode_size(101, &attrs).is_err());
}

#[test]
fn wire_response_ids_lengths_eof_and_failure_are_distinct() {
    let mut body = Vec::new();
    string(&mut body, b"new record\n");
    let mut response = vec![103];
    response.extend_from_slice(&7u32.to_be_bytes());
    response.extend_from_slice(&body);
    let (kind, payload) = decode_response(&response, 7).unwrap();
    assert_eq!(decode_data(kind, &payload, CHUNK).unwrap(), b"new record\n");
    assert!(decode_response(&response, 8).is_err());
    assert!(decode_response(&[], 7).is_err());
    assert!(decode_data(kind, &payload, 2).is_err());
    let mut trailing = payload.clone();
    trailing.push(0);
    assert!(decode_data(kind, &trailing, CHUNK).is_err());
    assert!(decode_data(103, &[0, 0, 0, 0], CHUNK).is_err());
    assert_eq!(decode_data(101, &1u32.to_be_bytes(), CHUNK).unwrap(), b"");
    assert!(decode_data(101, &3u32.to_be_bytes(), CHUNK).is_err());
    assert!(decode_data(101, &[], CHUNK).is_err());
}
