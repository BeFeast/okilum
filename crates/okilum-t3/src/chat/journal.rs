use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Environment, ManagerGateway, OutgoingMessage, SendReceipt};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MessageRecord {
    pub created_at_millis: u64,
    pub message: OutgoingMessage,
    pub receipt: Option<SendReceipt>,
}

#[derive(Clone, Debug)]
pub struct MessageJournal {
    directory: PathBuf,
    thread_id: String,
}

impl MessageJournal {
    /// `state` must be the application's durable state directory, outside
    /// the rebuildable index. The server origin and manager bind each intent.
    pub fn new(state: &Path, environment: &Environment, thread_id: &str) -> Self {
        let identity = serde_json::to_vec(&(environment.origin(), thread_id)).unwrap();
        let hash = format!("{:x}", Sha256::digest(identity));
        Self {
            directory: state.join("t3-messages").join(hash),
            thread_id: thread_id.into(),
        }
    }

    pub fn records(&self) -> Result<Vec<MessageRecord>> {
        let entries = match std::fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error).context("cannot read retained manager messages"),
        };
        let mut records = Vec::new();
        for entry in entries {
            let path = entry?.path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let record: MessageRecord = serde_json::from_slice(&std::fs::read(&path)?)
                .context("a retained manager message cannot be read; nothing was resent")?;
            self.validate(&record)?;
            records.push(record);
        }
        records.sort_by(|a, b| {
            (a.created_at_millis, &a.message.client_request_id)
                .cmp(&(b.created_at_millis, &b.message.client_request_id))
        });
        Ok(records)
    }

    /// Retain a unique identity before any network operation. A failed save
    /// never dispatches a request. Retrying reads this record, never mints an id.
    pub fn prepare(&self, text: &str) -> Result<MessageRecord> {
        ensure!(!text.trim().is_empty(), "the message is empty");
        // Refuse to mask damaged retained operations with a new message.
        self.records()?;
        std::fs::create_dir_all(&self.directory)?;
        let record = MessageRecord {
            created_at_millis: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis()
                .try_into()?,
            message: OutgoingMessage::queue(&self.thread_id, text),
            receipt: None,
        };
        let path = self.path(&record)?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        file.write_all(&serde_json::to_vec(&record)?)?;
        file.sync_all()?;
        sync_directory(&self.directory)?;
        Ok(record)
    }

    /// Send, or retry after an ambiguous network failure. An acknowledged
    /// message is returned locally without dispatching it a second time.
    pub fn send(
        &self,
        gateway: &dyn ManagerGateway,
        record: &MessageRecord,
    ) -> Result<MessageRecord> {
        self.validate(record)?;
        let disk: MessageRecord = serde_json::from_slice(&std::fs::read(self.path(record)?)?)?;
        self.validate(&disk)?;
        ensure!(
            disk.message == record.message,
            "the retained message identity changed; nothing was sent"
        );
        if disk.receipt.is_some() {
            return Ok(disk);
        }
        let receipt = gateway.send_message(&disk.message)?;
        ensure!(
            receipt.thread_id == self.thread_id,
            "T3 acknowledged another manager's message"
        );
        let acknowledged = MessageRecord {
            created_at_millis: disk.created_at_millis,
            message: disk.message,
            receipt: Some(receipt),
        };
        let temporary = self
            .directory
            .join(format!(".{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(&serde_json::to_vec(&acknowledged)?)?;
            file.sync_all()?;
            std::fs::rename(&temporary, self.path(&acknowledged)?)?;
            sync_directory(&self.directory)
        })();
        let _ = std::fs::remove_file(temporary);
        result?;
        Ok(acknowledged)
    }

    fn validate(&self, record: &MessageRecord) -> Result<()> {
        ensure!(
            record.message.thread_id == self.thread_id,
            "retained message belongs to another manager"
        );
        let key = &record.message.client_request_id;
        ensure!(
            key.starts_with("okilum-") && uuid::Uuid::parse_str(&key[7..]).is_ok(),
            "invalid retained message id"
        );
        Ok(())
    }

    fn path(&self, record: &MessageRecord) -> Result<PathBuf> {
        self.validate(record)?;
        Ok(self
            .directory
            .join(format!("{}.json", record.message.client_request_id)))
    }
}

fn sync_directory(directory: &Path) -> Result<()> {
    #[cfg(unix)]
    std::fs::File::open(directory)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = directory;
    Ok(())
}
