use super::*;
use crate::Environment;
use anyhow::{ensure, Result};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub command: Command,
    #[serde(deserialize_with = "required_sequence")]
    pub sequence: Option<u64>,
}
fn required_sequence<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<u64>, D::Error> {
    Option::<u64>::deserialize(deserializer)
}

fn semantic_target(intent: &Intent) -> Option<(&'static str, &str)> {
    match intent {
        Intent::Interrupt { run_id } => Some(("run", run_id)),
        Intent::Respond { request_id, .. } | Intent::Dismiss { request_id } => {
            Some(("request", request_id))
        }
        Intent::Message { .. } => None,
    }
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
    fn lock(&self) -> Result<File> {
        std::fs::create_dir_all(&self.directory)?;
        // All operations share this stable inode, never an atomically replaced
        // JSON record. Keep the file after releasing the advisory lock.
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.directory.join(".lock"))?;
        lock.lock()?;
        Ok(lock)
    }
    pub fn records(&self) -> Result<Vec<Record>> {
        let _lock = self.lock()?;
        self.records_unlocked()
    }
    fn records_unlocked(&self) -> Result<Vec<Record>> {
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
            let record: Record = serde_json::from_slice(&std::fs::read(&path)?).map_err(|_| {
                anyhow::anyhow!("Cannot read retained T3 command; nothing was resent")
            })?;
            ensure!(
                self.path(&record.command)? == path,
                "retained command identity changed"
            );
            ensure!(
                record
                    .sequence
                    .is_none_or(|sequence| sequence <= 9_007_199_254_740_991),
                "retained command acknowledgement is invalid; nothing was resent"
            );
            result.push(record);
        }
        result.sort_by(|a, b| a.command.command_id.cmp(&b.command.command_id));
        Ok(result)
    }
    /// Singleton run/request actions reuse their immutable retained identity.
    /// Messages require explicit retry while a previous message is uncertain;
    /// identical text after acknowledgement is still a distinct user message.
    pub fn prepare(&self, intent: Intent) -> Result<Record> {
        ensure!(
            intent.validate(),
            "invalid command intent; nothing was retained"
        );
        let _lock = self.lock()?;
        let records = self.records_unlocked()?;
        if let Some(target) = semantic_target(&intent) {
            let matching: Vec<_> = records
                .iter()
                .filter(|record| semantic_target(&record.command.intent) == Some(target))
                .collect();
            ensure!(
                matching.len() <= 1,
                "multiple retained actions target the same run or request; nothing was prepared"
            );
            if let Some(existing) = matching.first() {
                ensure!(existing.command.intent == intent,
                    "another retained action already targets this request; retry it or refresh before responding");
                return Ok((**existing).clone());
            }
        } else {
            ensure!(
                !records.iter().any(|record| record.sequence.is_none()
                    && matches!(record.command.intent, Intent::Message { .. })),
                "a retained message is unconfirmed; retry that message before preparing another"
            );
        }
        let record = Record {
            command: Command::new(&self.thread, intent),
            sequence: None,
        };
        let path = self.path(&record.command)?;
        self.publish(&path, &record, false)?;
        Ok(record)
    }
    /// Explicit retry only. The original complete command is read back from disk
    /// before every call. Acknowledged operations never hit the network again.
    /// Validate the whole journal: an unreadable entry could conceal another
    /// operation for this target, so even an unrelated-looking file blocks send.
    pub fn send(
        &self,
        record: &Record,
        dispatch: impl FnOnce(&Command) -> crate::stream::StreamResult<u64>,
    ) -> Result<Record> {
        let _lock = self.lock()?;
        let path = self.path(&record.command)?;
        let mut disk = self
            .records_unlocked()?
            .into_iter()
            .find(|disk| disk.command.command_id == record.command.command_id)
            .ok_or_else(|| anyhow::anyhow!("retained command is missing; nothing was sent"))?;
        ensure!(
            disk.command == record.command,
            "retained command changed; nothing was sent"
        );
        if disk.sequence.is_some() {
            return Ok(disk);
        }
        let sequence = dispatch(&disk.command)?;
        ensure!(
            sequence <= 9_007_199_254_740_991,
            "command acknowledgement is invalid; delivery remains unconfirmed"
        );
        disk.sequence = Some(sequence);
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
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Barrier,
    };
    #[test]
    fn attachment_only_delivery_retains_the_exact_asset_on_reopen_and_lost_ack() {
        use crate::stream::{AttachmentKind, AttachmentMetadata, ChatAttachment, StreamError};
        let directory = Directory::new();
        let environment = Environment::new("fixture", "http://localhost:9999").unwrap();
        let journal = Journal::new(directory.path(), &environment, "qa-thread");
        let attachment = ChatAttachment {
            id: "uploaded-asset".into(),
            metadata: AttachmentMetadata {
                kind: AttachmentKind::File,
                name: "report.txt".into(),
                mime_type: "text/plain".into(),
                size_bytes: 7,
            },
            source: None,
        };
        let record = journal
            .prepare(Intent::message_with_attachments(
                "".into(),
                DispatchMode::StartImmediately,
                vec![attachment],
            ))
            .unwrap();
        let original = record.command.wire().unwrap();
        assert_eq!(original["text"], "");
        assert_eq!(original["attachments"][0]["id"], "uploaded-asset");
        assert!(journal.send(&record, |_| Err(StreamError::Closed)).is_err());
        let reopened = Journal::new(directory.path(), &environment, "qa-thread");
        let retained = reopened.records().unwrap().pop().unwrap();
        assert_eq!(retained, record);
        let acknowledged = reopened
            .send(&retained, |command| {
                assert_eq!(command.wire().unwrap(), original);
                Ok(7)
            })
            .unwrap();
        assert_eq!(
            reopened
                .send(&acknowledged, |_| panic!(
                    "acknowledged attachment must not send again"
                ))
                .unwrap()
                .sequence,
            Some(7)
        );
        let legacy: Intent = serde_json::from_value(json!({"type":"message.dispatch","text":"old text-only draft","messageId":"legacy","dispatchMode":{"type":"start_immediately"}})).unwrap();
        assert!(
            Command::new("qa-thread", legacy).wire().unwrap()["attachments"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(journal
            .prepare(Intent::message("".into(), DispatchMode::StartImmediately))
            .is_err());
    }
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
    fn journal(dir: &Directory) -> Journal {
        Journal::new(
            dir.path(),
            &Environment::new("fixture", "http://127.0.0.1:8080").unwrap(),
            "thread",
        )
    }
    fn response(decision: &str) -> Intent {
        Intent::Respond {
            request_id: "request-1".into(),
            decision: Some(decision.into()),
            answers: None,
        }
    }

    #[test]
    fn concurrent_singleton_prepares_retain_one_command_and_conflicting_decisions_fail() {
        let dir = Directory::new();
        for intent in [
            response("accept"),
            Intent::Interrupt {
                run_id: "run-1".into(),
            },
        ] {
            let barrier = Arc::new(Barrier::new(3));
            let workers: Vec<_> = (0..2)
                .map(|_| {
                    let journal = journal(&dir);
                    let barrier = barrier.clone();
                    let intent = intent.clone();
                    std::thread::spawn(move || {
                        barrier.wait();
                        journal.prepare(intent).unwrap()
                    })
                })
                .collect();
            barrier.wait();
            let records: Vec<_> = workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect();
            assert_eq!(records[0], records[1]);
        }
        let journal = journal(&dir);
        assert_eq!(journal.records().unwrap().len(), 2);
        assert!(journal.prepare(response("decline")).is_err());
        assert!(journal
            .prepare(Intent::Dismiss {
                request_id: "request-1".into()
            })
            .is_err());
        let accepted = journal.prepare(response("accept")).unwrap();
        let acknowledged = journal.send(&accepted, |_| Ok(7)).unwrap();
        assert_eq!(journal.prepare(response("accept")).unwrap(), acknowledged);
        assert!(journal.prepare(response("decline")).is_err());
    }

    #[test]
    fn concurrent_sends_dispatch_once_and_return_the_same_persisted_receipt() {
        let dir = Directory::new();
        let record = journal(&dir).prepare(response("accept")).unwrap();
        let barrier = Arc::new(Barrier::new(3));
        let calls = Arc::new(AtomicUsize::new(0));
        let workers: Vec<_> = (0..2)
            .map(|_| {
                let journal = journal(&dir);
                let barrier = barrier.clone();
                let calls = calls.clone();
                let record = record.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    journal
                        .send(&record, |command| {
                            assert_eq!(command, &record.command);
                            calls.fetch_add(1, Ordering::SeqCst);
                            Ok(11)
                        })
                        .unwrap()
                })
            })
            .collect();
        barrier.wait();
        let records: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        assert_eq!(records[0], records[1]);
        assert_eq!(records[0].sequence, Some(11));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn lost_acknowledgement_reopens_singleton_ids_and_blocks_fresh_messages() {
        let dir = Directory::new();
        for intent in [
            response("accept"),
            Intent::Interrupt {
                run_id: "run-1".into(),
            },
            Intent::Dismiss {
                request_id: "request-2".into(),
            },
        ] {
            let original = journal(&dir).prepare(intent.clone()).unwrap();
            assert!(journal(&dir)
                .send(&original, |_| Err(crate::stream::StreamError::Closed))
                .is_err());
            let reopened = journal(&dir).prepare(intent).unwrap();
            assert_eq!(original, reopened);
            assert_eq!(
                original.command.wire().unwrap(),
                reopened.command.wire().unwrap()
            );
        }
        let original = journal(&dir)
            .prepare(Intent::message(
                "same".into(),
                DispatchMode::QueueAfterActive,
            ))
            .unwrap();
        assert!(journal(&dir)
            .send(&original, |_| Err(crate::stream::StreamError::Closed))
            .is_err());
        assert!(journal(&dir)
            .prepare(Intent::message(
                "same".into(),
                DispatchMode::QueueAfterActive
            ))
            .is_err());
        assert!(journal(&dir)
            .prepare(Intent::message(
                "different".into(),
                DispatchMode::QueueAfterActive
            ))
            .is_err());
        let retained = journal(&dir)
            .records()
            .unwrap()
            .into_iter()
            .find(|record| record.command == original.command)
            .unwrap();
        assert_eq!(retained, original);
        journal(&dir).send(&retained, |_| Ok(12)).unwrap();
        let next = journal(&dir)
            .prepare(Intent::message(
                "same".into(),
                DispatchMode::QueueAfterActive,
            ))
            .unwrap();
        assert_ne!(next.command.command_id, original.command.command_id);
        if let (
            Intent::Message {
                message_id: before, ..
            },
            Intent::Message {
                message_id: after, ..
            },
        ) = (&original.command.intent, &next.command.intent)
        {
            assert_ne!(before, after);
        } else {
            panic!("expected messages");
        }
    }

    #[test]
    fn concurrent_fresh_messages_allow_one_pending_intent_without_text_deduplication() {
        let dir = Directory::new();
        let barrier = Arc::new(Barrier::new(3));
        let workers: Vec<_> = (0..2)
            .map(|_| {
                let journal = journal(&dir);
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    journal.prepare(Intent::message(
                        "identical".into(),
                        DispatchMode::QueueAfterActive,
                    ))
                })
            })
            .collect();
        barrier.wait();
        let results: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(journal(&dir).records().unwrap().len(), 1);
    }

    #[test]
    fn damaged_other_record_blocks_pending_approval_without_overwriting_either() {
        let dir = Directory::new();
        let journal = journal(&dir);
        let stopped = journal
            .prepare(Intent::Interrupt {
                run_id: "finished-run".into(),
            })
            .unwrap();
        journal.send(&stopped, |_| Ok(7)).unwrap();
        let approval = journal.prepare(response("accept")).unwrap();
        let stop_path = journal.path(&stopped.command).unwrap();
        let approval_path = journal.path(&approval.command).unwrap();
        let stop_bytes = std::fs::read(&stop_path).unwrap();
        let approval_bytes = std::fs::read(&approval_path).unwrap();
        let damaged = b"unreadable old receipt";
        std::fs::write(&stop_path, damaged).unwrap();
        assert!(journal
            .send(&approval, |_| panic!(
                "unknown journal identity must block dispatch"
            ))
            .is_err());
        assert!(journal
            .prepare(Intent::Dismiss {
                request_id: "different-request".into()
            })
            .is_err());
        assert_eq!(std::fs::read(&stop_path).unwrap(), damaged);
        assert_eq!(std::fs::read(&approval_path).unwrap(), approval_bytes);
        // Positive control: restoring the same validated receipt permits the
        // original pending command, with no replacement identity.
        std::fs::write(stop_path, stop_bytes).unwrap();
        let acknowledged = journal
            .send(&approval, |command| {
                assert_eq!(command, &approval.command);
                Ok(8)
            })
            .unwrap();
        assert_eq!(acknowledged.sequence, Some(8));
    }

    #[test]
    fn corrupt_or_missing_receipts_are_preserved_and_never_dispatched() {
        let dir = Directory::new();
        let journal = journal(&dir);
        let record = journal.prepare(response("accept")).unwrap();
        let path = journal.path(&record.command).unwrap();
        let valid = std::fs::read(&path).unwrap();
        let mut missing: Value = serde_json::from_slice(&valid).unwrap();
        missing.as_object_mut().unwrap().remove("sequence");
        let mut unsafe_receipt: Value = serde_json::from_slice(&valid).unwrap();
        unsafe_receipt["sequence"] = json!(9_007_199_254_740_992u64);
        for bytes in [
            b"broken JSON".to_vec(),
            serde_json::to_vec(&missing).unwrap(),
            serde_json::to_vec(&unsafe_receipt).unwrap(),
        ] {
            std::fs::write(&path, &bytes).unwrap();
            assert!(journal.records().is_err());
            assert!(journal.prepare(response("accept")).is_err());
            assert!(journal
                .send(&record, |_| panic!("damaged receipt dispatched"))
                .is_err());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
        std::fs::write(&path, &valid).unwrap();
        assert!(journal
            .send(&record, |_| Ok(9_007_199_254_740_992))
            .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), valid);
        let acknowledged = journal
            .send(&record, |_| Ok(9_007_199_254_740_991))
            .unwrap();
        assert_eq!(
            acknowledged.sequence,
            Some(9_007_199_254_740_991),
            "positive control: valid receipt is retained"
        );
    }

    #[test]
    fn mismatched_retained_run_request_and_thread_cannot_dispatch() {
        let dir = Directory::new();
        let journal = journal(&dir);
        for intent in [
            response("accept"),
            Intent::Interrupt {
                run_id: "run-1".into(),
            },
        ] {
            let original = journal.prepare(intent).unwrap();
            let mut changed = original.clone();
            match &mut changed.command.intent {
                Intent::Respond { request_id, .. } => *request_id = "another-request".into(),
                Intent::Interrupt { run_id } => *run_id = "another-run".into(),
                _ => unreachable!(),
            }
            assert!(journal
                .send(&changed, |_| panic!("changed target dispatched"))
                .is_err());
            changed = original;
            changed.command.thread_id = "another-thread".into();
            assert!(journal
                .send(&changed, |_| panic!("wrong thread dispatched"))
                .is_err());
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
