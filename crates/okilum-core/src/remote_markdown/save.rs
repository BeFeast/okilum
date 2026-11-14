//! Explicit guarded OpenSSH save. No remote call is made by draft persistence.
use super::{Source, BYTE_LIMIT, DEADLINE};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::{self, BufRead, Read, Write},
    process::{Command, Stdio},
    sync::LazyLock,
};

pub const UNSUPPORTED: &str =
    "This server does not support safe saving; your changes are kept locally";
const PROGRAM: &str = include_str!("save.py");
const OUTPUT_LIMIT: usize = BYTE_LIMIT * 12 + 64 * 1024;
static COMMAND: LazyLock<String> = LazyLock::new(|| {
    // Only the fixed application program is shell-quoted. All user data is stdin.
    format!("python3 -I -c '{}'", PROGRAM.replace('\'', "'\\''"))
});

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revision {
    pub sha256: String,
    pub bytes: usize,
}
impl Revision {
    pub fn of(text: &str) -> Self {
        Self {
            sha256: format!("{:x}", Sha256::digest(text.as_bytes())),
            bytes: text.len(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub text: String,
    pub revision: Revision,
}
impl Snapshot {
    pub fn new(text: String) -> Self {
        Self {
            revision: Revision::of(&text),
            text,
        }
    }
    fn validate(&self) -> io::Result<()> {
        if self.text.len() > BYTE_LIMIT || self.revision != Revision::of(&self.text) {
            return Err(io::Error::other(
                "Invalid remote revision receipt; changes are kept locally",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Attempt {
    pub id: String,
    pub base: Revision,
    pub text: String,
}
impl Attempt {
    pub fn new(base: Revision, text: String) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            base,
            text,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    Saved {
        revision: Revision,
        already_present: bool,
        recovery: Vec<String>,
    },
    Conflict {
        remote: Option<Snapshot>,
        raced: Option<Snapshot>,
        recovery: Vec<String>,
        rollback_race: bool,
        message: String,
    },
    Unsupported {
        message: String,
    },
    Error {
        message: String,
    },
}
impl Outcome {
    pub fn validate(&self, attempt: &Attempt) -> io::Result<()> {
        match self {
            Self::Saved { revision, .. } if revision != &Revision::of(&attempt.text) => {
                return Err(io::Error::other("The server did not confirm this exact draft; Retry checks the remote version first"));
            }
            Self::Conflict { remote, raced, .. } => {
                if let Some(snapshot) = remote {
                    snapshot.validate()?;
                }
                if let Some(snapshot) = raced {
                    snapshot.validate()?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}
#[derive(Serialize)]
struct Request<'a> {
    version: u8,
    path_b64: String,
    text_b64: String,
    base: &'a Revision,
    attempt: &'a str,
}
fn request(source: &Source, attempt: &Attempt) -> io::Result<Vec<u8>> {
    if attempt.text.len() > BYTE_LIMIT
        || attempt.base.bytes > BYTE_LIMIT
        || uuid::Uuid::parse_str(&attempt.id).is_err()
    {
        return Err(io::Error::other(
            "Remote Markdown exceeds its save limit; changes are kept locally",
        ));
    }
    serde_json::to_vec(&Request {
        version: 1,
        path_b64: STANDARD.encode(source.target.remote_path()),
        text_b64: STANDARD.encode(attempt.text.as_bytes()),
        base: &attempt.base,
        attempt: &attempt.id,
    })
    .map_err(io::Error::other)
}

/// The caller must persist `attempt` before calling this on its existing worker.
pub fn save(
    source: &Source,
    attempt: &Attempt,
    cancelled: impl Fn() -> bool,
) -> io::Result<Outcome> {
    let clock = std::sync::Arc::new(crate::ssh_askpass::Clock::default());
    // The askpass session lives until `run` returns, i.e. past the child's
    // kill on cancel/deadline: its dialog closes and no stale prompt remains.
    let (command, askpass) = source
        .target
        .guarded_markdown_save_command(COMMAND.as_str(), &clock);
    let clock = askpass.as_ref().map(|_| clock);
    let outcome = run_with(command, clock, source, attempt, cancelled);
    drop(askpass);
    outcome
}
struct Process(std::process::Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
#[cfg(all(test, target_os = "linux"))]
fn run(
    command: Command,
    source: &Source,
    attempt: &Attempt,
    cancelled: impl Fn() -> bool,
) -> io::Result<Outcome> {
    run_with(command, None, source, attempt, cancelled)
}
fn run_with(
    mut command: Command,
    clock: Option<std::sync::Arc<crate::ssh_askpass::Clock>>,
    source: &Source,
    attempt: &Attempt,
    cancelled: impl Fn() -> bool,
) -> io::Result<Outcome> {
    let payload = request(source, attempt)?;
    if cancelled() {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Remote save cancelled; changes are kept locally",
        ));
    }
    let mut process = Process(
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    let mut input = process.0.stdin.take().unwrap();
    let mut output = process.0.stdout.take().unwrap();
    let mut errors = process.0.stderr.take().unwrap();
    let reader = std::thread::spawn(move || -> io::Result<Option<Vec<u8>>> {
        let mut output = io::BufReader::new(&mut output);
        let mut hello = Vec::new();
        output.by_ref().take(128).read_until(b'\n', &mut hello)?;
        if hello.is_empty() {
            return Err(io::Error::other(
                "The server disconnected before safe saving was ready; changes are kept locally",
            ));
        }
        if hello != b"{\"protocol\":\"okilum-safe-save-v1\"}\n" {
            return Ok(None); // SFTP-only banner: never send draft bytes.
        }
        input.write_all(&payload)?;
        drop(input);
        let mut bytes = Vec::new();
        output
            .take(OUTPUT_LIMIT as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > OUTPUT_LIMIT {
            Err(io::Error::other("Remote save receipt exceeds its bound"))
        } else {
            Ok(Some(bytes))
        }
    });
    let stderr = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = errors.by_ref().take(8192).read_to_end(&mut bytes);
        bytes
    });
    let deadline = crate::ssh_askpass::Deadline::after(DEADLINE, clock);
    let status = loop {
        if let Some(status) = process.0.try_wait()? {
            break status;
        }
        if cancelled() || deadline.expired() {
            let _ = process.0.kill();
            let _ = process.0.wait();
            return Err(io::Error::new(io::ErrorKind::TimedOut, "Remote save was interrupted. Changes are kept locally; Retry checks the remote version first"));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let receipt = reader
        .join()
        .map_err(|_| io::Error::other("Remote save receipt failed"))?;
    let errors = stderr.join().unwrap_or_default();
    if matches!(&receipt, Ok(None)) {
        return Ok(Outcome::Unsupported {
            message: UNSUPPORTED.into(),
        });
    }
    if !status.success() {
        let error = String::from_utf8_lossy(&errors).to_ascii_lowercase();
        if status.code() == Some(127)
            || error.contains("python3: not found")
            || error.contains("python3: command not found")
            || error.contains("only sftp")
            || error.contains("sftp connections only")
            || error.contains("subsystem request failed")
        {
            return Ok(Outcome::Unsupported {
                message: UNSUPPORTED.into(),
            });
        }
        return Err(io::Error::other("The server did not confirm the save. Changes are kept locally; Retry checks the remote version first"));
    }
    let Some(bytes) = receipt? else {
        return Ok(Outcome::Unsupported {
            message: UNSUPPORTED.into(),
        });
    };
    let outcome: Outcome = serde_json::from_slice(&bytes).map_err(|_| io::Error::other("The save receipt was lost or invalid. Changes are kept locally; Retry checks the remote version first"))?;
    outcome.validate(attempt)?;
    Ok(outcome)
}

#[cfg(all(test, target_os = "linux"))]
#[path = "save_tests.rs"]
mod tests;
