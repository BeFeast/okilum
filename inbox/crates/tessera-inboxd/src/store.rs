use std::{path::Path, time::Duration};

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use tessera_inbox_domain::{
    Capture, CaptureChange, CapturePage, CaptureResult, InvalidCapture, Item, ItemState, OwnerId,
};
use uuid::Uuid;

const SCHEMA_VERSION: i64 = 9;
const SCHEMA: &str = "
CREATE TABLE captures (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    owner_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    original_text TEXT NOT NULL,
    received_at_ms INTEGER NOT NULL CHECK(received_at_ms >= 0),
    UNIQUE(owner_id, item_id)
);
CREATE TABLE capture_operations (
    owner_id TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    PRIMARY KEY(owner_id, operation_id),
    FOREIGN KEY(owner_id, item_id) REFERENCES captures(owner_id, item_id)
);
CREATE INDEX captures_by_owner ON captures(owner_id, sequence);
PRAGMA user_version = 1;
";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid execution delivery transition")]
    InvalidExecutionTransition,
    #[error(transparent)]
    InvalidExecution(#[from] tessera_inbox_domain::execution::InvalidExecution),
    #[error("execution revision changed")]
    ExecutionRevisionConflict,
    #[error("invalid publication request")]
    InvalidPublication,
    #[error("destination conflicts with the saved publication")]
    PublicationConflict,
    #[error("fixture vault unavailable")]
    VaultUnavailable,
    #[error("discussion completion did not match a running operation")]
    InvalidDiscussionTransition,
    #[error("item not found")]
    MissingItem,
    #[error("invalid discussion request")]
    InvalidDiscussion,
    #[error("a discussion is already running")]
    DiscussionBusy,
    #[error("discussion context limit reached")]
    DiscussionLimit,
    #[error(transparent)]
    InvalidCapture(#[from] InvalidCapture),
    #[error("owner must not be nil")]
    InvalidOwner,
    #[error("server timestamp must be nonnegative")]
    InvalidTimestamp,
    #[error("operation identity is already bound to different content")]
    OperationConflict,
    #[error("item identity already exists")]
    ItemConflict,
    #[error("unsupported or foreign Inbox database")]
    UnsupportedDatabase,
    #[error("invalid capture cursor or page size")]
    InvalidPage,
    #[error("invalid persisted item identity")]
    InvalidStoredIdentity,
    #[error("Inbox storage failure")]
    Storage(#[from] rusqlite::Error),
}

pub struct Store {
    pub(crate) connection: Connection,
}

impl Store {
    /// The caller chooses a private durable directory outside any vault/cache.
    /// Never resets a corrupt, foreign or newer database.
    pub fn open(path: &Path) -> Result<Self, Error> {
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", true)?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        match version {
            0 => {
                let count: i64 = tx.query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",
                    [],
                    |r| r.get(0),
                )?;
                if count != 0 {
                    return Err(Error::UnsupportedDatabase);
                }
                tx.execute_batch(SCHEMA)?;
            }
            1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | SCHEMA_VERSION => {}
            _ => return Err(Error::UnsupportedDatabase),
        }
        if version < 2 {
            tx.execute_batch(
                "CREATE TABLE auth_owner (
                singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                owner_id TEXT NOT NULL,
                origin TEXT NOT NULL,
                passkey TEXT,
                bootstrap_hash TEXT,
                bootstrap_expires INTEGER
            ); PRAGMA user_version = 2;",
            )?;
        }
        if version < 3 {
            tx.execute_batch(crate::discussion::SCHEMA)?;
        }
        if version < 4 {
            tx.execute_batch(crate::publication::SCHEMA)?;
        }
        if version < 5 {
            tx.execute_batch("ALTER TABLE publications ADD COLUMN conflict TEXT CHECK(conflict = 'file_exists'); PRAGMA user_version=5;")?;
        }
        if version < 6 {
            tx.execute_batch(crate::execution::SCHEMA)?;
        }
        if version < 7 {
            tx.execute_batch(crate::questions::SCHEMA)?;
        }
        if version < 8 {
            tx.execute_batch("ALTER TABLE execution_questions ADD COLUMN observed_at INTEGER NOT NULL DEFAULT 0; PRAGMA user_version=8;")?;
        }
        if version < 9 {
            tx.execute_batch(crate::launch::SCHEMA)?;
        }
        tx.commit()?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        Ok(Self { connection })
    }

    /// Capture and its replay identity commit atomically. A lost response can
    /// replay after restart without inventing a new item or timestamp.
    pub fn capture(
        &mut self,
        owner: OwnerId,
        request: &Capture,
        now_ms: i64,
    ) -> Result<CaptureResult, Error> {
        valid_owner(owner)?;
        request.validate()?;
        if now_ms < 0 {
            return Err(Error::InvalidTimestamp);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let owner_key = owner.0.to_string();
        let operation_key = request.operation_id.to_string();
        let existing = tx
            .query_row(
                "SELECT c.item_id, c.original_text, c.received_at_ms FROM capture_operations o
                 JOIN captures c ON c.owner_id = o.owner_id AND c.item_id = o.item_id
                 WHERE o.owner_id = ?1 AND o.operation_id = ?2",
                params![owner_key, operation_key],
                read_item,
            )
            .optional()?;
        if let Some(row) = existing {
            let item = decode_item(row)?;
            if item.id != request.item_id || item.original_text != request.text {
                return Err(Error::OperationConflict);
            }
            return Ok(CaptureResult {
                operation_id: request.operation_id,
                item,
            });
        }
        let item_key = request.item_id.to_string();
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM captures WHERE owner_id = ?1 AND item_id = ?2)",
            params![owner_key, item_key],
            |r| r.get(0),
        )?;
        if exists {
            return Err(Error::ItemConflict);
        }
        tx.execute(
            "INSERT INTO captures(owner_id,item_id,original_text,received_at_ms) VALUES (?1,?2,?3,?4)",
            params![owner_key, item_key, request.text, now_ms],
        )?;
        tx.execute(
            "INSERT INTO capture_operations(owner_id,operation_id,item_id) VALUES (?1,?2,?3)",
            params![owner_key, operation_key, item_key],
        )?;
        tx.commit()?;
        Ok(CaptureResult {
            operation_id: request.operation_id,
            item: Item {
                id: request.item_id,
                original_text: request.text.clone(),
                received_at_ms: now_ms,
                state: ItemState::New,
                revision: 1,
            },
        })
    }

    pub fn item(&self, owner: OwnerId, id: Uuid) -> Result<Option<Item>, Error> {
        valid_owner(owner)?;
        self.connection
            .query_row(
                "SELECT item_id, original_text, received_at_ms FROM captures WHERE owner_id = ?1 AND item_id = ?2",
                params![owner.0.to_string(), id.to_string()],
                read_item,
            )
            .optional()?
            .map(decode_item)
            .transpose()
    }

    /// Capture-only snapshot pagination. New captures during paging are picked
    /// up by the next snapshot, not mixed into this one's stable upper boundary.
    pub fn captures(
        &self,
        owner: OwnerId,
        after: u64,
        through: Option<u64>,
        limit: u32,
    ) -> Result<CapturePage, Error> {
        valid_owner(owner)?;
        if !(1..=100).contains(&limit) || after > i64::MAX as u64 {
            return Err(Error::InvalidPage);
        }
        let owner_key = owner.0.to_string();
        let latest: u64 = self.connection.query_row(
            "SELECT coalesce(max(sequence), 0) FROM captures WHERE owner_id = ?1",
            [&owner_key],
            |r| r.get(0),
        )?;
        let through = through.unwrap_or(latest);
        if through > latest || after > through {
            return Err(Error::InvalidPage);
        }
        let mut statement = self.connection.prepare(
            "SELECT item_id, original_text, received_at_ms, sequence FROM captures
             WHERE owner_id = ?1 AND sequence > ?2 AND sequence <= ?3
             ORDER BY sequence LIMIT ?4",
        )?;
        let rows = statement.query_map(params![owner_key, after, through, limit + 1], |r| {
            Ok((read_item(r)?, r.get::<_, u64>(3)?))
        })?;
        let mut changes = Vec::new();
        for row in rows {
            let (item, sequence) = row?;
            changes.push(CaptureChange {
                sequence,
                item: decode_item(item)?,
            });
        }
        let has_more = changes.len() > limit as usize;
        changes.truncate(limit as usize);
        let next_after = if has_more {
            changes.last().map_or(after, |v| v.sequence)
        } else {
            through
        };
        Ok(CapturePage {
            changes,
            through,
            next_after,
            has_more,
        })
    }
}

fn valid_owner(owner: OwnerId) -> Result<(), Error> {
    if owner.0.is_nil() {
        return Err(Error::InvalidOwner);
    }
    Ok(())
}

type StoredItem = (String, String, i64);
fn read_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredItem> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
}
fn decode_item((id, original_text, received_at_ms): StoredItem) -> Result<Item, Error> {
    Ok(Item {
        id: Uuid::parse_str(&id).map_err(|_| Error::InvalidStoredIdentity)?,
        original_text,
        received_at_ms,
        state: ItemState::New,
        revision: 1,
    })
}
