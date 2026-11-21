//! #1390 SFTP follow against the real OpenSSH `sftp-server`. A stand-in `ssh`
//! on PATH execs the server directly (no network, keys or ssh config), so the
//! production `OpenSsh` transport speaks the real protocol to real files, and
//! killing the server process is a dropped connection as the client sees it.
//! One test function only: it changes this test binary's PATH.
#![cfg(unix)]
use okilum_core::log::sftp::{Follow, OpenSsh, Target, Transport, Update};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

fn sftp_server() -> Option<PathBuf> {
    [
        "/usr/lib/openssh/sftp-server",
        "/usr/lib/ssh/sftp-server",
        "/usr/libexec/sftp-server",
        "/usr/libexec/openssh/sftp-server",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|path| path.is_file())
}
fn line(n: u64) -> String {
    format!("line {n:07} {}\n", "x".repeat(40))
}
fn lines(range: std::ops::Range<u64>) -> String {
    range.map(line).collect()
}
fn append(path: &Path, text: &str) {
    OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
}
fn numbers(bytes: &[u8]) -> Vec<u64> {
    std::str::from_utf8(bytes)
        .unwrap()
        .lines()
        .map(|l| {
            assert_eq!(format!("{l}\n"), line(l[5..12].parse().unwrap()), "{l}");
            l[5..12].parse().unwrap()
        })
        .collect()
}
/// Poll until something other than data arrives; return the data and that.
fn drain<T: Transport>(follow: &mut Follow<T>) -> (Vec<u8>, Update) {
    let mut out = Vec::new();
    loop {
        match follow.poll() {
            Update::Records(bytes) => out.extend(bytes),
            Update::Pending => {}
            other => return (out, other),
        }
    }
}
fn connections(pids: &Path) -> Vec<String> {
    fs::read_to_string(pids)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}
fn target(path: &Path) -> Target {
    Target::parse(&format!("sftp://fixture{}", path.display())).unwrap()
}

#[test]
fn real_sftp_server_follow_reconnects_without_gaps_and_stops_on_rotation() {
    let Some(server) = sftp_server() else {
        eprintln!("SKIPPED: no OpenSSH sftp-server on this host");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let pids = dir.path().join("pids");
    let ssh = bin.join("ssh");
    fs::write(
        &ssh,
        format!(
            "#!/bin/sh\necho $$ >> '{}'\nexec '{}'\n",
            pids.display(),
            server.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![bin.clone()];
    paths.extend(std::env::split_paths(&path));
    std::env::set_var("PATH", std::env::join_paths(paths).unwrap());

    // A 3.3 MB log: the follow starts at a full line in its last 1 MiB.
    let log = dir.path().join("app.log");
    fs::write(&log, lines(0..60_000)).unwrap();
    let size = fs::metadata(&log).unwrap().len();
    let mut follow = Follow::new(target(&log), OpenSsh::default()).tail(1 << 20);
    let (bytes, end) = drain(&mut follow);
    assert_eq!(end, Update::CaughtUp);
    assert_eq!(follow.started_at(), size - (1 << 20));
    let seen = numbers(&bytes);
    assert!((15_000..20_000).contains(&seen.len()), "{}", seen.len());
    assert_eq!(*seen.last().unwrap(), 59_999);
    assert!(seen.windows(2).all(|w| w[1] == w[0] + 1));

    // Growth is followed on the same connection (EOF reopens the path only).
    append(&log, &lines(60_000..61_000));
    let (bytes, end) = drain(&mut follow);
    assert_eq!(end, Update::CaughtUp);
    assert_eq!(numbers(&bytes), (60_000..61_000).collect::<Vec<_>>());
    assert_eq!(connections(&pids).len(), 1);

    // Half a record arrives, then the connection is killed while the file
    // grows: the record is completed exactly once, nothing is skipped.
    let partial = line(61_000);
    let (head, rest) = partial.split_at(13);
    append(&log, head);
    assert_eq!(follow.poll(), Update::Pending);
    let pid = connections(&pids).pop().unwrap();
    assert!(Command::new("kill")
        .args(["-9", &pid])
        .status()
        .unwrap()
        .success());
    append(&log, rest);
    append(&log, &lines(61_001..62_000));
    assert!(matches!(follow.poll(), Update::Disconnected(_)));
    let (bytes, end) = drain(&mut follow);
    assert_eq!(end, Update::CaughtUp);
    assert_eq!(numbers(&bytes), (61_000..62_000).collect::<Vec<_>>());
    assert_eq!(
        connections(&pids).len(),
        2,
        "one new connection after the kill"
    );

    // Rotation: the next EOF reopen finds a new file and stops explicitly.
    fs::rename(&log, dir.path().join("app.log.1")).unwrap();
    fs::write(&log, lines(0..10)).unwrap();
    assert_eq!(follow.poll(), Update::ContinuityLost);
    assert_eq!(follow.poll(), Update::ContinuityLost);
    assert_eq!(connections(&pids).len(), 2);
    drop(follow);

    // copytruncate while attached: the new generation is never emitted.
    let copy = dir.path().join("copy.log");
    fs::write(&copy, lines(0..100)).unwrap();
    let mut follow = Follow::new(target(&copy), OpenSsh::default());
    assert_eq!(drain(&mut follow).1, Update::CaughtUp);
    append(&copy, &lines(100..101));
    assert_eq!(follow.poll(), Update::Records(line(100).into_bytes()));
    fs::write(&copy, lines(500..700)).unwrap();
    assert_eq!(follow.poll(), Update::ContinuityLost);
    println!(
        "real sftp-server follow: {} connections, tail start at byte {}",
        connections(&pids).len(),
        size - (1 << 20)
    );
}
