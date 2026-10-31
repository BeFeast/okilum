//! Durable, revision-checked native chat drafts, outside the rebuildable index.
//!
//! The generic store preserves opaque attachment payloads. The concrete native
//! attachment DTO keeps metadata only: local bytes and file handles remain
//! runtime-only, and restart requires reselecting every unuploaded attachment.
//! No blob staging or conversion of local IDs into server IDs happens here.
use crate::Environment;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fmt,
    fs::{File, OpenOptions},
    io::{self, Write},
    marker::PhantomData,
    path::{Path, PathBuf},
};

const VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    Image,
    File,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AttachmentState {
    Pending,
    Error {
        message: String,
    },
    NeedsReselect,
    Uploaded {
        attachment_id: String,
        environment_id: String,
    },
}

/// Persisted display metadata, never local bytes, file handles, signed URLs or
/// credentials. `source` is optional descriptive metadata; callers must exclude
/// signed URLs and credentials from that opaque value as well.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    pub id: String,
    pub kind: AttachmentKind,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<serde_json::Value>,
    pub state: AttachmentState,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Draft<A = serde_json::Value> {
    pub text: String,
    pub attachments: Vec<A>,
}
impl<A> Default for Draft<A> {
    fn default() -> Self {
        Self {
            text: String::new(),
            attachments: Vec::new(),
        }
    }
}

impl Draft<Attachment> {
    /// Restore after process restart. Only completed uploads for this exact
    /// server environment can be reused; all other rows need local reselect.
    /// Text, row order, local IDs and descriptive metadata remain unchanged.
    pub fn restore_for(&mut self, environment_id: &str) {
        for attachment in &mut self.attachments {
            let reusable = matches!(&attachment.state,
                AttachmentState::Uploaded { attachment_id, environment_id: uploaded_environment }
                    if !environment_id.trim().is_empty()
                        && !attachment_id.trim().is_empty()
                        && uploaded_environment == environment_id);
            if !reusable {
                attachment.state = AttachmentState::NeedsReselect;
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot<A = serde_json::Value> {
    pub revision: u64,
    pub draft: Draft<A>,
}

/// Error formatting deliberately excludes draft content and attachment metadata.
pub enum Error<A = serde_json::Value> {
    Io(io::ErrorKind),
    Malformed,
    UnsupportedVersion,
    WrongIdentity,
    RevisionExhausted,
    Conflict(Snapshot<A>),
}
impl<A> fmt::Display for Error<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(kind) => write!(f, "Cannot access retained chat draft ({kind:?})"),
            Self::Malformed => f.write_str("Retained chat draft cannot be decoded or encoded"),
            Self::UnsupportedVersion => f.write_str("Retained chat draft version is unsupported"),
            Self::WrongIdentity => {
                f.write_str("Retained chat draft belongs to another conversation")
            }
            Self::RevisionExhausted => f.write_str("Retained chat draft revision is exhausted"),
            Self::Conflict(_) => {
                f.write_str("Chat draft changed in another writer; local draft was not saved")
            }
        }
    }
}
impl<A> fmt::Debug for Error<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
impl<A> std::error::Error for Error<A> {}
impl<A> From<io::Error> for Error<A> {
    fn from(error: io::Error) -> Self {
        Self::Io(error.kind())
    }
}

#[derive(Serialize, Deserialize)]
struct Record<A> {
    version: u32,
    identity: (String, String, String),
    snapshot: Snapshot<A>,
}

#[derive(Clone)]
pub struct Store<A = serde_json::Value> {
    directory: PathBuf,
    identity: (String, String, String),
    attachment: PhantomData<fn() -> A>,
}
impl<A: Serialize + DeserializeOwned> Store<A> {
    /// `state` is application-owned durable state, never the disposable index.
    /// Environment aliases sharing an origin intentionally share drafts.
    pub fn new(state: &Path, environment: &Environment, project_id: &str, thread_id: &str) -> Self {
        let identity = (
            environment.origin(),
            project_id.to_owned(),
            thread_id.to_owned(),
        );
        let key = serde_json::to_vec(&identity).expect("string tuple serializes");
        Self {
            directory: state
                .join("t3-chat-drafts")
                .join(format!("{:x}", Sha256::digest(key))),
            identity,
            attachment: PhantomData,
        }
    }

    fn path(&self) -> PathBuf {
        self.directory.join("draft.json")
    }

    fn lock(&self) -> Result<File, Error<A>> {
        std::fs::create_dir_all(&self.directory)?;
        // Keep this inode stable: locking draft.json would fail to serialize
        // writers after atomic replacement. Never remove the lock file.
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.directory.join(".lock"))?;
        lock.lock()?;
        Ok(lock)
    }

    fn read(&self) -> Result<Snapshot<A>, Error<A>> {
        let bytes = match std::fs::read(self.path()) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Snapshot {
                    revision: 0,
                    draft: Draft::default(),
                })
            }
            Err(error) => return Err(error.into()),
        };
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| Error::Malformed)?;
        match value.get("version").and_then(serde_json::Value::as_u64) {
            Some(version) if version == u64::from(VERSION) => {}
            Some(_) => return Err(Error::UnsupportedVersion),
            None => return Err(Error::Malformed),
        }
        let record: Record<A> = serde_json::from_value(value).map_err(|_| Error::Malformed)?;
        if record.identity != self.identity {
            return Err(Error::WrongIdentity);
        }
        if record.snapshot.revision == 0 {
            return Err(Error::Malformed);
        }
        Ok(record.snapshot)
    }

    pub fn load(&self) -> Result<Snapshot<A>, Error<A>> {
        let _lock = self.lock()?;
        self.read()
    }

    /// Compare and save while holding a process-shared lock. On conflict the
    /// winning snapshot is returned intact; no draft or file is overwritten.
    pub fn save(&self, expected_revision: u64, draft: Draft<A>) -> Result<Snapshot<A>, Error<A>> {
        let _lock = self.lock()?;
        let current = self.read()?;
        if current.revision != expected_revision {
            return Err(Error::Conflict(current));
        }
        let snapshot = Snapshot {
            revision: expected_revision
                .checked_add(1)
                .ok_or(Error::RevisionExhausted)?,
            draft,
        };
        #[derive(Serialize)]
        struct BorrowedRecord<'a, A> {
            version: u32,
            identity: &'a (String, String, String),
            snapshot: &'a Snapshot<A>,
        }
        let bytes = serde_json::to_vec(&BorrowedRecord {
            version: VERSION,
            identity: &self.identity,
            snapshot: &snapshot,
        })
        .map_err(|_| Error::Malformed)?;
        self.publish(&bytes)?;
        Ok(snapshot)
    }

    /// Clearing retains a new revision, preventing stale writers from reviving
    /// a sent or explicitly discarded draft after an empty-state ABA cycle.
    pub fn clear(&self, expected_revision: u64) -> Result<Snapshot<A>, Error<A>> {
        self.save(expected_revision, Draft::default())
    }

    fn publish(&self, bytes: &[u8]) -> Result<(), Error<A>> {
        let path = self
            .directory
            .join(format!(".{}.tmp", uuid::Uuid::new_v4()));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        // This guard only exists after create_new succeeds, so cleanup cannot
        // remove any other writer's file, even if creation itself fails.
        struct OwnedTemporary {
            path: Option<PathBuf>,
            file: Option<File>,
        }
        impl Drop for OwnedTemporary {
            fn drop(&mut self) {
                // Windows requires closing our handle before unlinking.
                self.file.take();
                if let Some(path) = &self.path {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
        let mut temporary = OwnedTemporary {
            path: Some(path.clone()),
            file: Some(file),
        };
        let file = temporary.file.as_mut().expect("owned temporary file");
        file.write_all(bytes)?;
        file.sync_all()?;
        temporary.file.take();
        std::fs::rename(&path, self.path())?;
        temporary.path = None;
        #[cfg(unix)]
        File::open(&self.directory)?.sync_all()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("okilum-chat-drafts-{}", uuid::Uuid::new_v4())))
        }
        fn store(&self, origin: &str, project: &str, thread: &str) -> Store {
            Store::new(
                &self.0,
                &Environment::new("fixture", origin).unwrap(),
                project,
                thread,
            )
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn draft(text: &str) -> Draft {
        Draft {
            text: text.into(),
            attachments: vec![json!({"opaque":{"id":"local-1","name":"🧠.png"},"order":2})],
        }
    }

    #[test]
    fn typed_attachment_restart_restores_only_matching_completed_uploads() {
        let f = Fixture::new();
        let store: Store<Attachment> = Store::new(
            &f.0,
            &Environment::new("fixture", "http://127.0.0.1:8080").unwrap(),
            "project",
            "thread",
        );
        let states = vec![
            AttachmentState::Pending,
            AttachmentState::Error {
                message: "Upload interrupted".into(),
            },
            AttachmentState::NeedsReselect,
            AttachmentState::Uploaded {
                attachment_id: "server-1".into(),
                environment_id: "current".into(),
            },
            AttachmentState::Uploaded {
                attachment_id: "server-2".into(),
                environment_id: "another".into(),
            },
            AttachmentState::Uploaded {
                attachment_id: String::new(),
                environment_id: "current".into(),
            },
        ];
        let original = Draft {
            text: "Keep this text\n🧠".into(),
            attachments: states
                .into_iter()
                .enumerate()
                .map(|(i, state)| Attachment {
                    id: format!("local-{i}"),
                    kind: if i % 2 == 0 {
                        AttachmentKind::Image
                    } else {
                        AttachmentKind::File
                    },
                    name: format!("file-{i}"),
                    mime_type: "application/octet-stream".into(),
                    size_bytes: 42,
                    source: Some(json!({"kind":"picker","description":"user-selected"})),
                    state,
                })
                .collect(),
        };
        store.save(0, original.clone()).unwrap();
        let mut restored = store.load().unwrap().draft;
        restored.restore_for("current");
        assert_eq!(restored.text, original.text);
        for (i, (actual, before)) in restored
            .attachments
            .iter()
            .zip(&original.attachments)
            .enumerate()
        {
            let mut expected = before.clone();
            if i != 3 {
                expected.state = AttachmentState::NeedsReselect;
            }
            assert_eq!(*actual, expected);
        }
        let once = restored.clone();
        restored.restore_for("current");
        assert_eq!(restored, once, "restoration is idempotent");
        restored.restore_for("");
        assert!(restored
            .attachments
            .iter()
            .all(|attachment| attachment.state == AttachmentState::NeedsReselect));
    }

    #[test]
    fn restart_restores_text_and_opaque_attachments() {
        let f = Fixture::new();
        let store = f.store("http://127.0.0.1:8080", "project", "thread");
        assert_eq!(store.load().unwrap().revision, 0);
        let saved = store.save(0, draft("Keep this 🧠\nexactly")).unwrap();
        drop(store);
        assert_eq!(
            f.store("http://127.0.0.1:8080", "project", "thread")
                .load()
                .unwrap(),
            saved
        );
    }

    #[test]
    fn full_identity_isolated_and_origin_aliases_shared() {
        let f = Fixture::new();
        let original = f.store("http://127.0.0.1:8080", "project", "thread");
        let saved = original.save(0, draft("original")).unwrap();
        for other in [
            f.store("http://127.0.0.1:8081", "project", "thread"),
            f.store("http://127.0.0.1:8080", "another-project", "thread"),
            f.store("http://127.0.0.1:8080", "project", "another-thread"),
        ] {
            assert_eq!(other.load().unwrap().revision, 0);
            other.save(0, draft("independent")).unwrap();
        }
        let alias: Store = Store::new(
            &f.0,
            &Environment::new("another-alias", "http://127.0.0.1:8080/").unwrap(),
            "project",
            "thread",
        );
        assert_eq!(alias.load().unwrap(), saved);
        assert_eq!(original.load().unwrap(), saved);
    }

    #[test]
    fn two_writers_conflict_and_clear_cannot_cause_aba() {
        let f = Fixture::new();
        let a = f.store("http://127.0.0.1:8080", "project", "thread");
        let b = f.store("http://127.0.0.1:8080", "project", "thread");
        let old = b.load().unwrap();
        let saved = a.save(0, draft("winner")).unwrap();
        match b.save(old.revision, draft("loser")) {
            Err(Error::Conflict(winner)) => assert_eq!(winner, saved),
            other => panic!("expected conflict, got {other:?}"),
        }
        let empty = a.clear(saved.revision).unwrap();
        assert_eq!(empty.revision, 2);
        assert_eq!(empty.draft, Draft::default());
        for revision in [old.revision, saved.revision] {
            assert!(matches!(b.clear(revision), Err(Error::Conflict(s)) if s == empty));
            assert!(
                matches!(b.save(revision, draft("revived")), Err(Error::Conflict(s)) if s == empty)
            );
        }
        assert_eq!(b.load().unwrap(), empty);
    }

    #[test]
    fn concurrent_writers_publish_exactly_one_winner() {
        let f = Fixture::new();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let workers: Vec<_> = ["first", "second"]
            .into_iter()
            .map(|text| {
                let store = f.store("http://127.0.0.1:8080", "project", "thread");
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    store.save(0, draft(text))
                })
            })
            .collect();
        barrier.wait();
        let results: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        let winner = f
            .store("http://127.0.0.1:8080", "project", "thread")
            .load()
            .unwrap();
        assert_eq!(winner.revision, 1);
        for result in results {
            match result {
                Ok(snapshot) | Err(Error::Conflict(snapshot)) => assert_eq!(snapshot, winner),
                Err(error) => panic!("unexpected writer failure: {error}"),
            }
        }
    }

    #[test]
    fn malformed_unsupported_and_wrong_identity_records_are_preserved() {
        let f = Fixture::new();
        let store = f.store("http://127.0.0.1:8080", "project", "thread");
        let snapshot = store.save(0, draft("private content")).unwrap();
        let valid = std::fs::read(store.path()).unwrap();
        for bytes in [
            b"broken JSON".to_vec(),
            serde_json::to_vec(&json!({"version":2,"private":"content"})).unwrap(),
            serde_json::to_vec(&Record {
                version: VERSION,
                identity: ("another-origin".into(), "project".into(), "thread".into()),
                snapshot: snapshot.clone(),
            })
            .unwrap(),
        ] {
            std::fs::write(store.path(), &bytes).unwrap();
            assert!(store.load().is_err());
            assert!(store.save(snapshot.revision, draft("replacement")).is_err());
            assert!(store.clear(snapshot.revision).is_err());
            assert_eq!(std::fs::read(store.path()).unwrap(), bytes);
        }
        std::fs::write(store.path(), valid).unwrap();
        assert_eq!(
            store.load().unwrap(),
            snapshot,
            "positive control: valid data remains readable"
        );
        let error = Error::Conflict(snapshot);
        assert!(!format!("{error} {error:?}").contains("private content"));
    }

    #[test]
    fn actual_io_failure_preserves_obstruction_and_other_keys_work() {
        let f = Fixture::new();
        let blocked = f.store("http://127.0.0.1:8080", "project", "thread");
        std::fs::create_dir_all(blocked.path()).unwrap();
        let marker = blocked.path().join("preserve");
        std::fs::write(&marker, b"owned elsewhere").unwrap();
        assert!(matches!(blocked.load(), Err(Error::Io(_))));
        assert!(matches!(
            blocked.save(0, draft("cannot save")),
            Err(Error::Io(_))
        ));
        assert!(matches!(blocked.clear(0), Err(Error::Io(_))));
        assert_eq!(std::fs::read(&marker).unwrap(), b"owned elsewhere");
        let control = f.store("http://127.0.0.1:8080", "project", "control");
        let saved = control.save(0, draft("positive control")).unwrap();
        assert_eq!(control.load().unwrap(), saved);
        assert!(std::fs::read_dir(&blocked.directory)
            .unwrap()
            .all(|entry| entry
                .unwrap()
                .path()
                .extension()
                .is_none_or(|extension| extension != "tmp")));
    }
}
