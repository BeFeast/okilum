//! Explicit New thread creation. The caller durably retains the serialized
//! intent before dispatch; retries reuse its complete payload. No implicit retry,
//! first turn, executor launch or Area manifest mutation happens here.
use super::{NativeClient, StreamError, StreamResult};
use crate::{models, Environment, ModelSelection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fmt,
    fs::{File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

/// Durable create state errors never include retained content or peer payloads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JournalError {
    Io(io::ErrorKind),
    Malformed,
    UnsupportedVersion,
    WrongIdentity,
    InvalidReference,
    ChangedIntent,
    InvalidAcknowledgement,
}
impl fmt::Display for JournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(kind) => write!(f, "Cannot access retained New thread operation ({kind:?})"),
            Self::Malformed => {
                f.write_str("Retained New thread operation cannot be decoded or encoded")
            }
            Self::UnsupportedVersion => {
                f.write_str("Retained New thread operation version is unsupported")
            }
            Self::WrongIdentity => {
                f.write_str("Retained New thread operation belongs to another project or server")
            }
            Self::InvalidReference => {
                f.write_str("New thread reference is invalid for this project")
            }
            Self::ChangedIntent => {
                f.write_str("Retained New thread operation changed; acknowledgement was not saved")
            }
            Self::InvalidAcknowledgement => {
                f.write_str("New thread acknowledgement is invalid or changed")
            }
        }
    }
}
impl std::error::Error for JournalError {}
impl From<io::Error> for JournalError {
    fn from(error: io::Error) -> Self {
        Self::Io(error.kind())
    }
}
pub type JournalResult<T> = Result<T, JournalError>;

#[derive(Serialize, Deserialize)]
struct CreateRecord {
    version: u32,
    origin: String,
    project_id: String,
    intent: CreateIntent,
    #[serde(deserialize_with = "required_sequence")]
    sequence: Option<u64>,
}
fn required_sequence<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u64>, D::Error> {
    Option::<u64>::deserialize(deserializer)
}

/// One retained creation per origin/project. All windows reuse the same pending
/// identity until its acknowledgement is durably saved. This journal performs
/// no network calls and does not automatically retry pending operations.
#[derive(Clone)]
pub struct Journal {
    directory: PathBuf,
    environment: Environment,
    project_id: String,
}
impl Journal {
    /// `state` must be the application's durable state, outside the index.
    pub fn new(state: &Path, environment: &Environment, project_id: &str) -> Self {
        let key = serde_json::to_vec(&(environment.origin(), project_id))
            .expect("string tuple serializes");
        Self {
            directory: state
                .join("t3-thread-creates")
                .join(format!("{:x}", Sha256::digest(key))),
            environment: environment.clone(),
            project_id: project_id.into(),
        }
    }
    fn path(&self) -> PathBuf {
        self.directory.join("create.json")
    }
    fn lock(&self) -> JournalResult<File> {
        if self.project_id.trim().is_empty() {
            return Err(JournalError::InvalidReference);
        }
        std::fs::create_dir_all(&self.directory)?;
        // Never lock the atomically replaced record inode or delete this file.
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.directory.join(".lock"))?;
        file.lock()?;
        Ok(file)
    }
    fn read(&self) -> JournalResult<Option<CreateRecord>> {
        let bytes = match std::fs::read(self.path()) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| JournalError::Malformed)?;
        match value.get("version").and_then(Value::as_u64) {
            Some(1) => {}
            Some(_) => return Err(JournalError::UnsupportedVersion),
            None => return Err(JournalError::Malformed),
        }
        let record: CreateRecord =
            serde_json::from_value(value).map_err(|_| JournalError::Malformed)?;
        if record.origin != self.environment.origin()
            || record.project_id != self.project_id
            || record.intent.origin != record.origin
            || record.intent.project_id != record.project_id
        {
            return Err(JournalError::WrongIdentity);
        }
        if !record.intent.valid()
            || record
                .sequence
                .is_some_and(|sequence| sequence > 9_007_199_254_740_991)
        {
            return Err(JournalError::Malformed);
        }
        Ok(Some(record))
    }
    pub fn pending(&self) -> JournalResult<Option<CreateIntent>> {
        let _lock = self.lock()?;
        Ok(self
            .read()?
            .filter(|record| record.sequence.is_none())
            .map(|record| record.intent))
    }
    /// Reuse an unconfirmed operation exactly, including after restart. Only a
    /// durably acknowledged predecessor permits publication of new identities.
    pub fn prepare(
        &self,
        reference: &models::Snapshot,
        title: &str,
        created_at: &str,
    ) -> JournalResult<CreateIntent> {
        if reference.target.project_id != self.project_id
            || reference.target.thread_id.trim().is_empty()
            || reference.configuration.thread_id != reference.target.thread_id
        {
            return Err(JournalError::InvalidReference);
        }
        let _lock = self.lock()?;
        if let Some(pending) = self.read()?.filter(|record| record.sequence.is_none()) {
            return Ok(pending.intent);
        }
        let intent = CreateIntent::new(&self.environment, reference, title, created_at)
            .map_err(|_| JournalError::InvalidReference)?;
        self.publish(&CreateRecord {
            version: 1,
            origin: self.environment.origin(),
            project_id: self.project_id.clone(),
            intent: intent.clone(),
            sequence: None,
        })?;
        Ok(intent)
    }
    /// Persist only an acknowledgement for the exact currently retained intent.
    /// Repeating the same acknowledgement is harmless; stale windows cannot
    /// acknowledge a later operation or replace an existing receipt.
    pub fn acknowledge(&self, intent: &CreateIntent, sequence: u64) -> JournalResult<()> {
        if sequence > 9_007_199_254_740_991 {
            return Err(JournalError::InvalidAcknowledgement);
        }
        let _lock = self.lock()?;
        let mut record = self.read()?.ok_or(JournalError::ChangedIntent)?;
        if record.intent != *intent {
            return Err(JournalError::ChangedIntent);
        }
        if let Some(saved) = record.sequence {
            return if saved == sequence {
                Ok(())
            } else {
                Err(JournalError::InvalidAcknowledgement)
            };
        }
        record.sequence = Some(sequence);
        self.publish(&record)
    }
    fn publish(&self, record: &CreateRecord) -> JournalResult<()> {
        let bytes = serde_json::to_vec(record).map_err(|_| JournalError::Malformed)?;
        let path = self
            .directory
            .join(format!(".{}.tmp", uuid::Uuid::new_v4()));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        struct OwnedTemporary {
            path: Option<PathBuf>,
            file: Option<File>,
        }
        impl Drop for OwnedTemporary {
            fn drop(&mut self) {
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
        file.write_all(&bytes)?;
        file.sync_all()?;
        temporary.file.take();
        std::fs::rename(&path, self.path())?;
        temporary.path = None;
        #[cfg(unix)]
        File::open(&self.directory)?.sync_all()?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateIntent {
    version: u32,
    origin: String,
    reference_thread_id: String,
    command_id: String,
    thread_id: String,
    project_id: String,
    title: String,
    model_selection: ModelSelection,
    runtime_mode: String,
    interaction_mode: String,
    created_at: String,
}

impl CreateIntent {
    /// Copy the explicit project and confirmed model/runtime settings from the
    /// reference conversation. `created_at` is retained local operation metadata;
    /// the server assigns the conversation's canonical creation time.
    pub fn new(
        environment: &Environment,
        reference: &models::Snapshot,
        title: &str,
        created_at: &str,
    ) -> StreamResult<Self> {
        if reference.configuration.thread_id != reference.target.thread_id {
            return Err(StreamError::Protocol("invalid New thread reference"));
        }
        reference
            .validate(&reference.configuration.model_selection)
            .map_err(|_| StreamError::Protocol("invalid New thread model selection"))?;
        let intent = Self {
            version: 1,
            origin: environment.origin(),
            reference_thread_id: reference.target.thread_id.clone(),
            command_id: uuid::Uuid::new_v4().to_string(),
            thread_id: uuid::Uuid::new_v4().to_string(),
            project_id: reference.target.project_id.clone(),
            title: title.trim().to_owned(),
            model_selection: reference.configuration.model_selection.clone(),
            runtime_mode: reference.configuration.runtime_mode.clone(),
            interaction_mode: reference.configuration.interaction_mode.clone(),
            created_at: created_at.to_owned(),
        };
        intent.wire()?;
        Ok(intent)
    }

    pub fn command_id(&self) -> &str {
        &self.command_id
    }
    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }
    pub fn project_id(&self) -> &str {
        &self.project_id
    }
    pub fn created_at(&self) -> &str {
        &self.created_at
    }

    fn reference(&self) -> models::Target {
        models::Target {
            thread_id: self.reference_thread_id.clone(),
            project_id: self.project_id.clone(),
        }
    }

    fn valid(&self) -> bool {
        self.version == 1
            && !self.origin.trim().is_empty()
            && !self.project_id.trim().is_empty()
            && !self.reference_thread_id.trim().is_empty()
            && !self.title.trim().is_empty()
            && self.title.trim() == self.title
            && !self.created_at.trim().is_empty()
            && self.created_at.trim() == self.created_at
            && uuid::Uuid::parse_str(&self.command_id).is_ok()
            && uuid::Uuid::parse_str(&self.thread_id).is_ok()
            && !self.model_selection.instance_id.trim().is_empty()
            && !self.model_selection.model.trim().is_empty()
            && matches!(
                self.runtime_mode.as_str(),
                "approval-required" | "auto-accept-edits" | "auto" | "full-access"
            )
            && matches!(self.interaction_mode.as_str(), "default" | "plan")
    }

    pub fn wire(&self) -> StreamResult<Value> {
        if !self.valid() {
            return Err(StreamError::Protocol("invalid retained New thread intent"));
        }
        Ok(json!({
            "type": "thread.create",
            "createdBy": "user",
            "creationSource": "web",
            "commandId": self.command_id,
            "threadId": self.thread_id,
            "projectId": self.project_id,
            "title": self.title,
            "modelSelection": self.model_selection,
            "runtimeMode": self.runtime_mode,
            "interactionMode": self.interaction_mode,
            "branch": null,
            "worktreePath": null,
        }))
    }
}

impl NativeClient {
    /// One bounded attempt. Recheck membership and model availability using the
    /// explicit reference thread before sending the unchanged retained payload.
    /// A lost acknowledgement leaves the caller's intent unconfirmed; this
    /// method never mints replacement identities or retries automatically.
    pub fn create_thread(&self, intent: &CreateIntent) -> StreamResult<u64> {
        let payload = intent.wire()?;
        if !self.access_allowed() {
            return Err(StreamError::Closed);
        }
        if intent.origin != self.environment.origin() {
            return Err(StreamError::WrongOrigin);
        }
        if !self.can_operate() {
            return Err(StreamError::Rejected);
        }
        let reference = self.manager_models(&intent.reference())?;
        if reference.target != intent.reference()
            || reference.configuration.thread_id != intent.reference_thread_id
        {
            return Err(StreamError::Protocol(
                "New thread reference no longer matches its project",
            ));
        }
        reference
            .validate(&intent.model_selection)
            .map_err(|_| StreamError::Rejected)?;
        let response = self.manager_rpc("orchestration.dispatchCommand", payload, false)?;
        response["sequence"]
            .as_u64()
            .filter(|sequence| *sequence <= 9_007_199_254_740_991)
            .ok_or(StreamError::Protocol("invalid New thread acknowledgement"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            Self(
                std::env::temp_dir()
                    .join(format!("okilum-thread-creates-{}", uuid::Uuid::new_v4())),
            )
        }
        fn journal(&self) -> Journal {
            Journal::new(&self.0, &environment(), "bound-project")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn environment() -> Environment {
        Environment::new("fixture", "http://127.0.0.1:8080").unwrap()
    }
    fn reference() -> models::Snapshot {
        models::Snapshot {
            target: models::Target { project_id: "bound-project".into(), thread_id: "existing-manager".into() },
            catalog: serde_json::from_value(json!({
                "runtimeMode":"approval-required",
                "providers":[{"providerInstanceId":"codex","driverKind":"codex","models":[{"id":"model-1"}]}]
            })).unwrap(),
            configuration: serde_json::from_value(json!({
                "threadId":"existing-manager", "modelSelection":{"instanceId":"codex","model":"model-1"},
                "runtimeMode":"approval-required", "interactionMode":"plan"
            })).unwrap(),
        }
    }
    fn intent() -> CreateIntent {
        CreateIntent::new(
            &environment(),
            &reference(),
            "New thread",
            "2026-10-31T12:00:00Z",
        )
        .unwrap()
    }

    #[test]
    fn lost_acknowledgement_and_restart_reuse_the_exact_pending_intent() {
        let f = Fixture::new();
        assert!(f.journal().pending().unwrap().is_none());
        let original = f
            .journal()
            .prepare(&reference(), "New thread", "2026-10-31T12:00:00Z")
            .unwrap();
        // A lost network response never reaches acknowledge. A new window and
        // later timestamps must not mint another command or thread identity.
        let reopened = f.journal();
        assert_eq!(reopened.pending().unwrap(), Some(original.clone()));
        let retried = reopened
            .prepare(&reference(), "Changed title", "2026-10-31T13:00:00Z")
            .unwrap();
        assert_eq!(retried, original);
        assert_eq!(retried.wire().unwrap(), original.wire().unwrap());
    }

    #[test]
    fn confirmed_operation_allows_new_ids_and_rejects_stale_acknowledgement() {
        let f = Fixture::new();
        let journal = f.journal();
        let first = journal
            .prepare(&reference(), "New thread", "2026-10-31T12:00:00Z")
            .unwrap();
        journal.acknowledge(&first, 7).unwrap();
        journal.acknowledge(&first, 7).unwrap();
        assert_eq!(
            journal.acknowledge(&first, 8),
            Err(JournalError::InvalidAcknowledgement)
        );
        assert!(f.journal().pending().unwrap().is_none());
        let next = f
            .journal()
            .prepare(&reference(), "New thread", "2026-10-31T13:00:00Z")
            .unwrap();
        assert_ne!(first.command_id(), next.command_id());
        assert_ne!(first.thread_id(), next.thread_id());
        assert_eq!(
            journal.acknowledge(&first, 7),
            Err(JournalError::ChangedIntent)
        );
        assert_eq!(journal.pending().unwrap(), Some(next));
    }

    #[test]
    fn concurrent_windows_share_one_pending_creation() {
        let f = Fixture::new();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let workers: Vec<_> = ["First", "Second"]
            .into_iter()
            .map(|title| {
                let journal = f.journal();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    journal
                        .prepare(&reference(), title, "2026-10-31T12:00:00Z")
                        .unwrap()
                })
            })
            .collect();
        barrier.wait();
        let prepared: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        assert_eq!(prepared[0], prepared[1]);
        assert_eq!(f.journal().pending().unwrap(), Some(prepared[0].clone()));
    }

    #[test]
    fn journals_isolate_origin_and_project_but_share_origin_aliases() {
        let f = Fixture::new();
        let original = f
            .journal()
            .prepare(&reference(), "Original", "2026-10-31T12:00:00Z")
            .unwrap();
        let other_origin = Journal::new(
            &f.0,
            &Environment::new("fixture", "http://127.0.0.1:8081").unwrap(),
            "bound-project",
        );
        assert!(other_origin.pending().unwrap().is_none());
        let independent = other_origin
            .prepare(&reference(), "Another", "2026-10-31T12:00:00Z")
            .unwrap();
        assert_ne!(independent.command_id(), original.command_id());
        let other_project = Journal::new(&f.0, &environment(), "other-project");
        assert!(other_project.pending().unwrap().is_none());
        assert_eq!(
            other_project.prepare(&reference(), "Wrong project", "2026-10-31T12:00:00Z"),
            Err(JournalError::InvalidReference)
        );
        let mut other_reference = reference();
        other_reference.target.project_id = "other-project".into();
        let another = other_project
            .prepare(&other_reference, "Another", "2026-10-31T12:00:00Z")
            .unwrap();
        other_project.acknowledge(&another, 9).unwrap();
        let alias = Journal::new(
            &f.0,
            &Environment::new("another-alias", "http://127.0.0.1:8080/").unwrap(),
            "bound-project",
        );
        assert_eq!(alias.pending().unwrap(), Some(original));
    }

    #[test]
    fn corrupt_or_unsupported_create_state_is_never_skipped_or_overwritten() {
        let f = Fixture::new();
        let journal = f.journal();
        let original = journal
            .prepare(&reference(), "Private draft", "2026-10-31T12:00:00Z")
            .unwrap();
        let valid = std::fs::read(journal.path()).unwrap();
        let mut missing_receipt: Value = serde_json::from_slice(&valid).unwrap();
        missing_receipt.as_object_mut().unwrap().remove("sequence");
        let mut wrong_identity: Value = serde_json::from_slice(&valid).unwrap();
        wrong_identity["project_id"] = json!("another-project");
        for bytes in [
            b"broken JSON".to_vec(),
            serde_json::to_vec(&json!({"version":2})).unwrap(),
            serde_json::to_vec(&missing_receipt).unwrap(),
            serde_json::to_vec(&wrong_identity).unwrap(),
        ] {
            std::fs::write(journal.path(), &bytes).unwrap();
            assert!(journal.pending().is_err());
            assert!(journal
                .prepare(&reference(), "Replacement", "2026-10-31T13:00:00Z")
                .is_err());
            assert!(journal.acknowledge(&original, 1).is_err());
            assert_eq!(std::fs::read(journal.path()).unwrap(), bytes);
        }
        std::fs::write(journal.path(), valid).unwrap();
        assert_eq!(
            journal.pending().unwrap(),
            Some(original),
            "positive control: valid state remains readable"
        );
    }

    #[test]
    fn protocol_payload_retains_configuration_and_has_no_turn_or_reference_mutation() {
        let intent = intent();
        assert_eq!(
            intent.wire().unwrap(),
            json!({
                "type":"thread.create", "createdBy":"user", "creationSource":"web", "commandId":intent.command_id(), "threadId":intent.thread_id(),
                "projectId":"bound-project", "title":"New thread",
                "modelSelection":{"instanceId":"codex","model":"model-1"},
                "runtimeMode":"approval-required", "interactionMode":"plan",
                "branch":null, "worktreePath":null
            })
        );
        assert_ne!(intent.thread_id(), "existing-manager");
        assert_ne!(intent.command_id(), intent.thread_id());
        assert_eq!(intent.reference(), reference().target);
    }

    #[test]
    fn retained_restart_and_retry_keep_exact_payload_and_identities() {
        let original = intent();
        let bytes = serde_json::to_vec(&original).unwrap();
        let restored: CreateIntent = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(restored, original);
        assert_eq!(restored.wire().unwrap(), original.wire().unwrap());
        assert_eq!(restored.wire().unwrap(), restored.wire().unwrap());
        let another = intent();
        assert_ne!(another.command_id(), original.command_id());
        assert_ne!(another.thread_id(), original.thread_id());
    }

    #[test]
    fn invalid_or_empty_reference_cannot_create_an_intent() {
        let mut reference = reference();
        reference.target.project_id = " ".into();
        assert!(CreateIntent::new(
            &environment(),
            &reference,
            "New thread",
            "2026-10-31T12:00:00Z"
        )
        .is_err());
        reference.target.project_id = "bound-project".into();
        reference.target.thread_id = "foreign-thread".into();
        assert!(CreateIntent::new(
            &environment(),
            &reference,
            "New thread",
            "2026-10-31T12:00:00Z"
        )
        .is_err());
        reference.target.thread_id = "existing-manager".into();
        reference.configuration.runtime_mode = "invalid-mode".into();
        assert!(CreateIntent::new(
            &environment(),
            &reference,
            "New thread",
            "2026-10-31T12:00:00Z"
        )
        .is_err());
        assert!(CreateIntent::new(
            &environment(),
            &super::tests::reference(),
            " ",
            "2026-10-31T12:00:00Z"
        )
        .is_err());
    }

    #[test]
    fn malformed_retained_target_is_rejected_before_dispatch() {
        for field in [
            "projectId",
            "referenceThreadId",
            "commandId",
            "threadId",
            "createdAt",
        ] {
            let mut value = serde_json::to_value(intent()).unwrap();
            value[field] = json!("");
            let altered: CreateIntent = serde_json::from_value(value).unwrap();
            assert!(altered.wire().is_err(), "invalid {field} accepted");
        }
    }
}
