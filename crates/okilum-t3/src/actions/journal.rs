use super::*;
use crate::Environment;
use anyhow::{ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub command: Command,
    pub sequence: Option<u64>,
}
#[derive(Clone)]
pub struct Journal {
    directory: PathBuf,
    thread: String,
}
impl Journal {
    /// Durable operational state, deliberately outside the disposable index and
    /// separate from the pre-native MCP journal. No migration or cross-protocol retry.
    pub fn new(state: &Path, environment: &Environment, thread: &str) -> Self {
        let key = serde_json::to_vec(&(environment.origin(), thread)).unwrap();
        Self {
            directory: state
                .join("t3-native-commands")
                .join(format!("{:x}", Sha256::digest(key))),
            thread: thread.into(),
        }
    }
    fn path(&self, command: &Command) -> Result<PathBuf> {
        command.wire()?;
        ensure!(
            command.thread_id == self.thread,
            "retained command belongs to another manager"
        );
        Ok(self.directory.join(format!("{}.json", command.command_id)))
    }
    pub fn records(&self) -> Result<Vec<Record>> {
        let entries = match std::fs::read_dir(&self.directory) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut result = Vec::new();
        for e in entries {
            let path = e?.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let record: Record = serde_json::from_slice(&std::fs::read(&path)?)
                .context("Cannot read retained T3 command; nothing was resent")?;
            ensure!(
                self.path(&record.command)? == path,
                "retained command identity changed"
            );
            result.push(record);
        }
        result.sort_by(|a, b| a.command.command_id.cmp(&b.command.command_id));
        Ok(result)
    }
    pub fn prepare(&self, intent: Intent) -> Result<Record> {
        self.records()?;
        let record = Record {
            command: Command::new(&self.thread, intent),
            sequence: None,
        };
        let path = self.path(&record.command)?;
        std::fs::create_dir_all(&self.directory)?;
        self.publish(&path, &record, false)?;
        Ok(record)
    }
    /// Explicit retry only. The original complete command is read back from disk
    /// before every call. Acknowledged operations never hit the network again.
    pub fn send(
        &self,
        record: &Record,
        dispatch: impl FnOnce(&Command) -> crate::stream::StreamResult<u64>,
    ) -> Result<Record> {
        let path = self.path(&record.command)?;
        let mut disk: Record = serde_json::from_slice(&std::fs::read(&path)?)?;
        ensure!(
            disk.command == record.command,
            "retained command changed; nothing was sent"
        );
        if disk.sequence.is_some() {
            return Ok(disk);
        }
        disk.sequence = Some(dispatch(&disk.command)?);
        self.publish(&path, &disk, true)?;
        Ok(disk)
    }
    fn publish(&self, path: &Path, record: &Record, replace: bool) -> Result<()> {
        let tmp = self
            .directory
            .join(format!(".{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut f = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&tmp)?;
            f.write_all(&serde_json::to_vec(record)?)?;
            f.sync_all()?;
            if replace {
                std::fs::rename(&tmp, path)?;
            } else {
                std::fs::hard_link(&tmp, path)?;
            }
            #[cfg(unix)]
            std::fs::File::open(&self.directory)?.sync_all()?;
            Ok(())
        })();
        let _ = std::fs::remove_file(tmp);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            Self(
                std::env::temp_dir()
                    .join(format!("okilum-native-actions-{}", uuid::Uuid::new_v4())),
            )
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn uncertain_delivery_reopens_the_same_intent_and_acknowledgement_prevents_resend() {
        let dir = Directory::new();
        let env = Environment::new("fixture", "http://127.0.0.1:8080").unwrap();
        let journal = Journal::new(dir.path(), &env, "thread");
        let record = journal
            .prepare(Intent::message(
                "hello 🧠".into(),
                DispatchMode::SteerActive {
                    target_run_id: "original-run".into(),
                },
            ))
            .unwrap();
        let wire = record.command.wire().unwrap();
        assert!(journal
            .send(&record, |c| {
                assert_eq!(c.wire().unwrap(), wire);
                Err(crate::stream::StreamError::Closed)
            })
            .is_err());
        let reopened = Journal::new(dir.path(), &env, "thread");
        let retained = reopened.records().unwrap().pop().unwrap();
        assert_eq!(retained, record);
        let acknowledged = reopened
            .send(&retained, |c| {
                assert_eq!(c.wire().unwrap(), wire);
                Ok(17)
            })
            .unwrap();
        assert_eq!(acknowledged.sequence, Some(17));
        assert_eq!(
            reopened
                .send(&retained, |_| panic!(
                    "acknowledged command dispatched twice"
                ))
                .unwrap(),
            acknowledged
        );
        let mut changed = retained;
        changed.command.intent = Intent::message(
            "same identity, changed content".into(),
            DispatchMode::QueueAfterActive,
        );
        assert!(reopened
            .send(&changed, |_| panic!("changed identity dispatched"))
            .is_err());
        let wrong = Journal::new(dir.path(), &env, "other-thread");
        assert!(wrong
            .send(&record, |_| panic!("cross-thread dispatch"))
            .is_err());
        let other_env = Environment::new("other", "http://127.0.0.1:8081").unwrap();
        assert!(Journal::new(dir.path(), &other_env, "thread")
            .records()
            .unwrap()
            .is_empty());
    }
    #[test]
    fn damaged_state_and_failed_prepare_never_dispatch_or_mask_an_older_intent() {
        let dir = Directory::new();
        let env = Environment::new("fixture", "http://127.0.0.1:8080").unwrap();
        let journal = Journal::new(dir.path(), &env, "thread");
        let record = journal
            .prepare(Intent::Interrupt {
                run_id: "run".into(),
            })
            .unwrap();
        std::fs::write(journal.path(&record.command).unwrap(), b"broken").unwrap();
        assert!(journal
            .prepare(Intent::message(
                "new".into(),
                DispatchMode::QueueAfterActive
            ))
            .is_err());
        assert!(journal
            .send(&record, |_| panic!("damaged intent dispatched"))
            .is_err());
    }
}
