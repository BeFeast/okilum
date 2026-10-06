//! Finite, bounded transport for the same-executable macOS pasteboard reader.
use gpui_component::input::clipboard::ClipboardReadError as Error;
use rustix::fs::{fcntl_getfl, fcntl_setfl, OFlags};
use std::{
    io::{ErrorKind, Read},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

pub(super) const MAX_BYTES: usize = 8 * 1024 * 1024;
pub(super) const TIMEOUT: Duration = Duration::from_secs(2);

struct Reap(Child);
impl Drop for Reap {
    fn drop(&mut self) {
        // Kill is harmless after a successful wait; wait also reaps every early return.
        let _ = self.0.kill();
        let status = self.0.wait();
        #[cfg(all(test, target_os = "linux"))]
        tests::REAPED.with(|reaped| {
            *reaped.borrow_mut() = Some((self.0.id(), status));
        });
        #[cfg(not(all(test, target_os = "linux")))]
        let _ = status;
    }
}

pub(super) fn read(
    command: &mut Command,
    deadline: Instant,
    cancelled: impl Fn() -> bool,
) -> Result<Option<String>, Error> {
    if cancelled() {
        return Err(Error::Cancelled);
    }
    if Instant::now() >= deadline {
        return Err(Error::Timeout);
    }
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| Error::Io)?;
    let mut child = Reap(child);
    let mut stdout = child.0.stdout.take().ok_or(Error::Io)?;
    let flags = fcntl_getfl(&stdout).map_err(|_| Error::Io)?;
    fcntl_setfl(&stdout, flags | OFlags::NONBLOCK).map_err(|_| Error::Io)?;
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut eof = false;
    loop {
        if cancelled() {
            return Err(Error::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(Error::Timeout);
        }
        if !eof {
            match stdout.read(&mut buffer) {
                Ok(0) => eof = true,
                Ok(n) => {
                    // One status byte precedes an optional exact UTF-8 payload.
                    if bytes.len().saturating_add(n) > MAX_BYTES + 1 {
                        return Err(Error::TooLarge);
                    }
                    bytes.extend_from_slice(&buffer[..n]);
                    continue;
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(_) => return Err(Error::Io),
            }
        }
        if let Some(status) = child.0.try_wait().map_err(|_| Error::Io)? {
            if !status.success() {
                return Err(Error::Io);
            }
            if eof {
                if cancelled() {
                    return Err(Error::Cancelled);
                }
                return decode(&bytes);
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn decode(bytes: &[u8]) -> Result<Option<String>, Error> {
    let (&status, payload) = bytes.split_first().ok_or(Error::Io)?;
    if status == 0 {
        return String::from_utf8(payload.to_vec())
            .map(Some)
            .map_err(|_| Error::InvalidUtf8);
    }
    if !payload.is_empty() {
        return Err(Error::Io);
    }
    match status {
        1 => Ok(None),
        2 => Err(Error::TooLarge),
        3 => Err(Error::InvalidUtf8),
        4 => Err(Error::ChangedOffer),
        _ => Err(Error::Io),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Capture the actual child handle's wait result, not a reusable /proc PID.
    // read and its RAII cleanup are synchronous; parallel tests use separate slots.
    #[cfg(target_os = "linux")]
    thread_local! {
        pub(super) static REAPED: std::cell::RefCell<Option<(u32, std::io::Result<std::process::ExitStatus>)>> = const { std::cell::RefCell::new(None) };
    }
    fn shell(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]);
        command
    }
    #[test]
    fn status_protocol_preserves_empty_text_and_exact_utf8() {
        let mut command = shell("printf '\\000a\\r\\nПривет 🧠\\n'");
        assert_eq!(
            read(&mut command, Instant::now() + TIMEOUT, || false).unwrap(),
            Some("a\r\nПривет 🧠\n".into())
        );
        assert_eq!(decode(&[0]), Ok(Some(String::new())));
        assert_eq!(decode(&[1]), Ok(None));
        assert_eq!(decode(&[0, 255]), Err(Error::InvalidUtf8));
        assert_eq!(decode(&[4]), Err(Error::ChangedOffer));
        assert_eq!(decode(&[1, 0]), Err(Error::Io));
    }
    #[cfg(target_os = "linux")]
    fn child_refusal(script: &str, cancel: bool, expected: Error) {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("reader.pid");
        // Publish only after printf closes the complete PID file. Cancellation
        // must not kill the shell between redirection's create and its write.
        let mut command = shell(&format!(
            "printf '%s\\n' \"$$\" > \"$TESSERA_TEST_PID.tmp\" && mv -- \"$TESSERA_TEST_PID.tmp\" \"$TESSERA_TEST_PID\" && {script}"
        ));
        command.env("TESSERA_TEST_PID", &marker);
        let start = Instant::now();
        let duration = if expected == Error::Timeout {
            Duration::from_millis(250)
        } else {
            TIMEOUT
        };
        REAPED.with(|reaped| *reaped.borrow_mut() = None);
        let published_pid = || {
            let text = std::fs::read_to_string(&marker).ok()?;
            text.strip_suffix('\n')?
                .parse::<u32>()
                .ok()
                .filter(|pid| *pid > 0)
        };
        let result = read(&mut command, start + duration, || {
            cancel && published_pid().is_some()
        });
        assert_eq!(result.as_ref(), Err(&expected));
        let pid = published_pid().expect("the child must publish a complete positive PID");
        let (reaped_pid, status) = REAPED
            .with(|reaped| reaped.borrow_mut().take())
            .expect("read must run the child cleanup before returning");
        assert_eq!(
            reaped_pid, pid,
            "observe the spawned reader, not another child"
        );
        let status = status.expect("wait must successfully reap the reader");
        if matches!(expected, Error::Timeout | Error::Cancelled) {
            use std::os::unix::process::ExitStatusExt as _;
            assert_eq!(
                status.signal(),
                Some(9),
                "the sleeping reader must be killed"
            );
        }
        std::fs::remove_file(marker).unwrap();
        assert!(start.elapsed() < Duration::from_secs(3));
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn timeout_and_cancellation_kill_and_reap_the_reader() {
        for cancel in [false, true] {
            child_refusal(
                "exec sleep 30",
                cancel,
                if cancel {
                    Error::Cancelled
                } else {
                    Error::Timeout
                },
            );
        }
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn oversize_stream_is_bounded_and_reader_is_reaped() {
        child_refusal("exec head -c 8388610 /dev/zero", false, Error::TooLarge);
    }
    #[test]
    fn missing_status_and_failed_child_do_not_become_empty_paste() {
        for script in ["exit 0", "printf '\\000text'; exit 1"] {
            assert_eq!(
                read(&mut shell(script), Instant::now() + TIMEOUT, || false),
                Err(Error::Io)
            );
        }
    }
}
