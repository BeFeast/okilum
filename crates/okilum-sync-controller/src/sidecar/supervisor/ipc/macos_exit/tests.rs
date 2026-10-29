use super::*;
use crate::sidecar::supervisor::ipc::code_requirement::CodeRequirement;
use std::{
    io::{Read, Write},
    os::unix::net::UnixListener,
    process::{Child, Command, ExitStatus, Stdio},
    time::Duration,
};

const FIXTURE: &str = "sidecar::supervisor::ipc::macos_exit::tests::exit_peer_fixture";

#[test]
#[ignore = "subprocess fixture selected explicitly by the exit-watch tests"]
fn exit_peer_fixture() -> Result<()> {
    let path = std::env::var_os("OKILUM_EXIT_FIXTURE").context("fixture socket missing")?;
    let mut stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.write_all(b"ready")?;
    let mut command = [0u8];
    stream.read_exact(&mut command)?;
    ensure!(command == [b'x'], "unexpected fixture command");
    Ok(())
}

struct OwnedChild(Child);
impl OwnedChild {
    fn finish(&mut self) -> Result<ExitStatus> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(status) = self.0.try_wait()? {
                return Ok(status);
            }
            ensure!(
                Instant::now() < deadline,
                "owned fixture exit deadline elapsed"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn child_peer() -> Result<(tempfile::TempDir, OwnedChild, UnixStream, SignedPeer)> {
    let root = tempfile::tempdir_in("/tmp")?;
    let socket = root.path().canonicalize()?.join("peer.sock");
    let listener = UnixListener::bind(&socket)?;
    listener.set_nonblocking(true)?;
    let executable = std::env::current_exe()?;
    let output = Command::new("codesign")
        .arg("-dvvv")
        .arg(&executable)
        .output()?;
    ensure!(
        output.status.success(),
        "cannot inspect fixture code signature"
    );
    let signature = String::from_utf8(output.stderr)?;
    let hash = signature
        .lines()
        .find_map(|line| line.strip_prefix("CDHash="))
        .context("fixture code has no cdhash")?;
    let peer = SignedPeer::new(
        CodeRequirement::code_directory_hash(hash.trim())?,
        &executable,
    )?;
    let mut child = OwnedChild(
        Command::new(executable)
            .args(["--ignored", "--exact", FIXTURE, "--test-threads=1"])
            .env_clear()
            .env("OKILUM_EXIT_FIXTURE", socket)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                ensure!(
                    child.0.try_wait()?.is_none(),
                    "fixture exited before connecting"
                );
                ensure!(
                    Instant::now() < deadline,
                    "fixture connection deadline elapsed"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => return Err(error.into()),
        }
    };
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut ready = [0u8; 5];
    stream.read_exact(&mut ready)?;
    ensure!(
        ready == *b"ready",
        "fixture did not execute its expected test"
    );
    Ok((root, child, stream, peer))
}

#[test]
fn authenticated_child_exit_is_observed_with_an_alive_positive_control() -> Result<()> {
    let (_root, mut child, mut stream, peer) = child_peer()?;
    let mut watch = ExitWatch::capture(&stream, &peer)?;
    assert_eq!(watch.pid, child.0.id() as usize);
    let flags = unsafe { libc::fcntl(watch.queue.as_raw_fd(), libc::F_GETFD) };
    assert!(flags >= 0 && flags & libc::FD_CLOEXEC != 0);
    assert!(!watch.wait(Instant::now() + Duration::from_millis(10))?);
    assert!(
        child.0.try_wait()?.is_none(),
        "timeout did not observe a live control"
    );
    stream.write_all(b"x")?;
    assert!(watch.wait(Instant::now() + Duration::from_secs(2))?);
    assert!(child.finish()?.success());
    assert!(
        watch.wait(Instant::now())?,
        "confirmed exit was not retained"
    );
    assert!(
        ExitWatch::capture(&stream, &peer).is_err(),
        "dead peer became a fresh process watch"
    );
    Ok(())
}

#[test]
fn wrong_signed_peer_is_refused_and_dropping_a_watch_does_not_stop_the_child() -> Result<()> {
    let (_root, mut child, mut stream, peer) = child_peer()?;
    let wrong = SignedPeer::new(
        CodeRequirement::team_and_identifier("ABCDE12345", "org.example.wrong")?,
        &std::env::current_exe()?,
    )?;
    assert!(ExitWatch::capture(&stream, &wrong).is_err());
    assert!(child.0.try_wait()?.is_none());
    let watch = ExitWatch::capture(&stream, &peer)?;
    drop(watch);
    assert!(
        child.0.try_wait()?.is_none(),
        "dropping an observer killed its peer"
    );
    stream.write_all(b"x")?;
    assert!(child.finish()?.success());
    Ok(())
}
