use super::*;
use std::{cell::RefCell, rc::Rc};

#[derive(Default)]
struct Remote {
    data: Vec<u8>,
    fail_at: Option<u64>,
    opens: usize,
    disconnects: usize,
    short: bool,
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
        Ok(remote.data[start..remote.data.len().min(start + size)].to_vec())
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
    remote.borrow_mut().fail_at = Some(8);
    assert!(matches!(follow.poll(), Update::Disconnected(_)));
    assert_eq!(follow.offset(), 8);
    assert_eq!(follow.poll(), Update::Records(b"three\nfour\n".to_vec()));
    assert_eq!(follow.poll(), Update::CaughtUp);
    assert_eq!(remote.borrow().opens, 4);
    assert_eq!(remote.borrow().disconnects, 4);
}

#[test]
fn partial_utf8_record_survives_disconnect_and_short_reads() {
    let record = "שלום🙂\n".as_bytes();
    let cut = record.len() - 3;
    let (mut follow, remote) = setup(&record[..cut]);
    assert_eq!(follow.poll(), Update::Pending);
    remote.borrow_mut().fail_at = Some(0);
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
fn full_prefix_proof_detects_rewrite_outside_boundary_guards() {
    let mut data = vec![b'x'; CHUNK * 2 + 5];
    data.push(b'\n');
    let (mut follow, remote) = setup(&data);
    while follow.offset() < data.len() as u64 {
        follow.poll();
    }
    remote.borrow_mut().data[CHUNK / 2] = b'y';
    assert_eq!(follow.poll(), Update::ContinuityLost);
}

#[test]
fn failed_prefix_verification_never_changes_partial_or_emitted_cursor() {
    let data = vec![b'x'; CHUNK + 5];
    let (mut follow, remote) = setup(&data);
    follow.poll();
    follow.poll();
    remote.borrow_mut().fail_at = Some(CHUNK as u64);
    assert!(matches!(follow.poll(), Update::Disconnected(_)));
    assert_eq!(follow.offset(), data.len() as u64);
    remote.borrow_mut().data.extend_from_slice(b"\nnext\n");
    let mut expected = data;
    expected.extend_from_slice(b"\nnext\n");
    assert_eq!(follow.poll(), Update::Records(expected));
}

#[test]
fn history_limit_is_explicit_and_no_more_network_reads_occur() {
    let (mut follow, remote) = setup(b"");
    follow.consumed = vec![b'x'; HISTORY_LIMIT];
    assert_eq!(follow.poll(), Update::LimitReached);
    assert_eq!(remote.borrow().opens, 0);
    assert_eq!(follow.offset(), HISTORY_LIMIT as u64);
}

#[test]
fn urls_are_not_shell_commands_and_never_include_credentials() {
    let target = Target::parse("sftp://home_alias/var/log/a%20b%3B%24%28id%29.log").unwrap();
    assert_eq!(target.path, b"/var/log/a b;$(id).log");
    let command = target.command();
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
    let command = target.command();
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
    let markdown = Target::parse_login("sftp://alias/notes/a.md")
        .unwrap()
        .command();
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
    let log = Target::parse("sftp://alias/var/log/a.log")
        .unwrap()
        .command();
    let args = log
        .get_args()
        .map(|a| a.to_str().unwrap())
        .collect::<Vec<_>>();
    assert!(args.contains(&"-oBatchMode=yes"));
    assert_eq!(log.get_envs().count(), 0);
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
