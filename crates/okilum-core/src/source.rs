//! Lossless source storage for the opt-in AI Brain POC (Unix hosts).
//!
//! A brain has one configured operational directory, shared by every cooperating
//! writer. Its lock serializes operations across threads/processes, not external
//! editors. `Managed` is an explicit assertion that writers/rename operations are
//! excluded during commit. Without that assertion we retain proposals only.
//! The operational directory is durable, outside the brain and derived indexes;
//! never delete it as an index rebuild. Existing reader APIs remain unchanged.
use base64::{engine::general_purpose::STANDARD, Engine};
use rustix::fs::{open, openat, renameat, Mode, OFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use uuid::Uuid;

pub const SCHEMA: &str = "ai-brain/v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteBoundary {
    /// Caller establishes a managed brain with all writers using this store's
    /// operational directory; unmanaged writers must not race the commit.
    Managed,
    /// No exclusion established: persist a proposal; never replace source.
    Unmanaged,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceSnapshot {
    pub schema: String,
    pub brain_id: String,
    pub path: String,
    pub revision: String,
    pub content_base64: String,
    pub media_type: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceWrite {
    pub schema: String,
    pub operation_id: String,
    pub brain_id: String,
    pub path: String,
    pub expected_revision: Option<String>,
    pub content_base64: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WriteOutcome {
    Written,
    Unchanged,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WriteReceipt {
    pub operation_id: String,
    pub path: String,
    pub previous_revision: Option<String>,
    pub revision: String,
    pub outcome: WriteOutcome,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceConflict {
    pub conflict_id: String,
    pub path: String,
    pub expected_revision: Option<String>,
    pub current_revision: Option<String>,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    NotFound,
    InvalidPath,
    InvalidRequest,
    Conflict,
    IoError,
    Indeterminate,
}

#[derive(Debug)]
pub struct SourceError {
    pub code: ErrorCode,
    pub message: String,
    pub conflict: Option<Box<SourceConflict>>,
}
impl std::fmt::Display for SourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}
impl std::error::Error for SourceError {}
impl From<std::io::Error> for SourceError {
    fn from(e: std::io::Error) -> Self {
        error(ErrorCode::IoError, e.to_string())
    }
}
impl From<rustix::io::Errno> for SourceError {
    fn from(e: rustix::io::Errno) -> Self {
        Self::from(std::io::Error::from(e))
    }
}
type Result<T> = std::result::Result<T, SourceError>;
fn error(code: ErrorCode, message: impl Into<String>) -> SourceError {
    SourceError {
        code,
        message: message.into(),
        conflict: None,
    }
}
fn revision(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

/// Durable recovery material. `request` retains proposed bytes, preimage retains
/// the version we observed, and later divergent observations are kept as well.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<SourceSnapshot>,
    pub request: SourceWrite,
    pub preimage_base64: Option<String>,
    pub previous_revision: Option<String>,
    pub receipt: Option<WriteReceipt>,
    pub conflict: Option<SourceConflict>,
    pub divergent_observations_base64: Vec<Option<String>>,
}

/// An inspectable conflict and the latest source revision to guard resolution.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConflictView {
    pub conflict: SourceConflict,
    pub base: Option<SourceSnapshot>,
    pub current: Option<SourceSnapshot>,
    pub proposed: SourceSnapshot,
}

// Only a read optimization: every entry is checked against filesystem identity
// under writer.lock. Recovery records remain authoritative; no format migration
// or additional durable index is required, including with older cooperating writers.
#[derive(PartialEq, Eq)]
struct JournalIdentity(u64, u64, u64, i64, i64, i64, i64);
impl JournalIdentity {
    fn read(path: &Path) -> Result<Self> {
        let m = fs::metadata(path)?;
        Ok(Self(
            m.dev(),
            m.ino(),
            m.len(),
            m.mtime(),
            m.mtime_nsec(),
            m.ctime(),
            m.ctime_nsec(),
        ))
    }
}
struct JournalSummary {
    identity: JournalIdentity,
    pending_path: Option<String>,
}

/// Required operational capability. Older binaries reject the extra binding key
/// at open; upgraded handles also fence every write under writer.lock.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RequiredProposalFeed {
    pub capability: String,
    pub epoch: String,
    pub activation_id: String,
    pub policy_version: u32,
    pub active: bool,
}
impl RequiredProposalFeed {
    pub fn validate(&self) -> Result<()> {
        if self.capability != "tessera-proposal-feed/v1"
            || self.policy_version == 0
            || Uuid::parse_str(&self.epoch)
                .map(|v| v.to_string())
                .ok()
                .as_deref()
                != Some(&self.epoch)
            || Uuid::parse_str(&self.activation_id)
                .map(|v| v.to_string())
                .ok()
                .as_deref()
                != Some(&self.activation_id)
        {
            return Err(error(
                ErrorCode::InvalidRequest,
                "invalid required proposal feed binding",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, PartialOrd)]
enum ProposalSupport {
    None,
    Feed,
    Drafts,
    Adoption,
    InboxAdoption,
    Generation,
    Retry,
    PublicAdoption,
    SuggestionsControl,
}

pub struct SourceStore {
    brain_id: String,
    binding: serde_json::Value,
    required_feed: Option<RequiredProposalFeed>,
    read_only: bool,
    drafts_supported: bool,
    adoption_supported: bool,
    inbox_adoption_supported: bool,
    required_inbox_adoption: bool,
    required_adoption: bool,
    required_drafts: bool,
    required_terminal: bool,
    required_generation: bool,
    required_retry: bool,
    retry_supported: bool,
    public_adoption_supported: bool,
    required_public_adoption: bool,
    suggestions_supported: bool,
    required_suggestions: bool,
    generation_supported: bool,
    pub(crate) root: File,
    pub(crate) root_path: PathBuf,
    state: PathBuf,
    journal_summaries: Mutex<HashMap<String, JournalSummary>>,
    #[cfg(test)]
    journal_reads: std::sync::atomic::AtomicUsize,
    pub(crate) boundary: WriteBoundary,
}

impl SourceStore {
    /// The operational directory must exist outside the brain; this method never
    /// picks a hidden index directory or guesses a root. Its contents are private
    /// operational records, including original/proposed note bytes.
    pub fn open(
        brain_id: &str,
        root: &Path,
        state: &Path,
        boundary: WriteBoundary,
    ) -> Result<Self> {
        Self::open_supported(
            brain_id,
            root,
            state,
            boundary,
            ProposalSupport::None,
            false,
        )
    }

    /// Explicit capability for the feed-aware Runner/maintenance path. This does
    /// not enroll a brain. Caller must preserve feed state before any writes.
    pub fn open_with_proposal_feed(
        brain_id: &str,
        root: &Path,
        state: &Path,
        boundary: WriteBoundary,
    ) -> Result<Self> {
        Self::open_supported(
            brain_id,
            root,
            state,
            boundary,
            ProposalSupport::Feed,
            false,
        )
    }

    /// Preserving proposal draft/disposition writer. Merely opening never enrolls.
    pub fn open_with_proposals(
        brain_id: &str,
        root: &Path,
        state: &Path,
        boundary: WriteBoundary,
    ) -> Result<Self> {
        Self::open_supported(
            brain_id,
            root,
            state,
            boundary,
            ProposalSupport::Drafts,
            false,
        )
    }

    /// Preserving context-adoption coordinator. Opening does not enroll a brain.
    pub fn open_with_proposal_adoption(
        brain_id: &str,
        root: &Path,
        state: &Path,
        boundary: WriteBoundary,
    ) -> Result<Self> {
        Self::open_supported(
            brain_id,
            root,
            state,
            boundary,
            ProposalSupport::Adoption,
            false,
        )
    }

    /// Preserves both context and delegated Inbox proposal adoption.
    pub fn open_with_proposal_inbox_adoption(
        brain_id: &str,
        root: &Path,
        state: &Path,
        boundary: WriteBoundary,
    ) -> Result<Self> {
        Self::open_supported(
            brain_id,
            root,
            state,
            boundary,
            ProposalSupport::InboxAdoption,
            false,
        )
    }

    /// Preserves frozen provider attempts; enrollment remains explicit.
    pub fn open_with_proposal_generation(
        brain_id: &str,
        root: &Path,
        state: &Path,
        boundary: WriteBoundary,
    ) -> Result<Self> {
        Self::open_supported(
            brain_id,
            root,
            state,
            boundary,
            ProposalSupport::Generation,
            false,
        )
    }

    /// Preserves accepted Retry receipts and immutable attempt history.
    pub fn open_with_proposal_retry(
        brain_id: &str,
        root: &Path,
        state: &Path,
        boundary: WriteBoundary,
    ) -> Result<Self> {
        Self::open_supported(
            brain_id,
            root,
            state,
            boundary,
            ProposalSupport::Retry,
            false,
        )
    }

    /// Preserves public adoption terminal receipts alongside original targets.
    pub fn open_with_public_proposal_adoption(
        brain_id: &str,
        root: &Path,
        state: &Path,
        boundary: WriteBoundary,
    ) -> Result<Self> {
        Self::open_supported(
            brain_id,
            root,
            state,
            boundary,
            ProposalSupport::PublicAdoption,
            false,
        )
    }

    /// Preserves product-owned suggestion activation and pause state.
    pub fn open_with_suggestions_control(
        brain_id: &str,
        root: &Path,
        state: &Path,
        boundary: WriteBoundary,
    ) -> Result<Self> {
        Self::open_supported(
            brain_id,
            root,
            state,
            boundary,
            ProposalSupport::SuggestionsControl,
            false,
        )
    }

    /// Feed-aware retrieval may read enrolled sources but cannot mutate them.
    pub fn open_read_only(brain_id: &str, root: &Path, state: &Path) -> Result<Self> {
        Self::open_supported(
            brain_id,
            root,
            state,
            WriteBoundary::Unmanaged,
            ProposalSupport::SuggestionsControl,
            true,
        )
    }

    fn open_supported(
        brain_id: &str,
        root: &Path,
        state: &Path,
        boundary: WriteBoundary,
        support: ProposalSupport,
        read_only: bool,
    ) -> Result<Self> {
        Uuid::parse_str(brain_id)
            .map_err(|_| error(ErrorCode::InvalidRequest, "invalid brain UUID"))?;
        if fs::symlink_metadata(root)?.file_type().is_symlink()
            || fs::symlink_metadata(state)?.file_type().is_symlink()
        {
            return Err(error(
                ErrorCode::InvalidPath,
                "root/state cannot be symlinks",
            ));
        }
        let root_path = fs::canonicalize(root)?;
        let state = fs::canonicalize(state)?;
        if state.starts_with(&root_path) || root_path.starts_with(&state) {
            return Err(error(
                ErrorCode::InvalidPath,
                "operational directory must be separate from brain",
            ));
        }
        let mut store = Self {
            binding: serde_json::Value::Null,
            required_feed: None,
            read_only,
            drafts_supported: support >= ProposalSupport::Drafts,
            adoption_supported: support >= ProposalSupport::Adoption,
            inbox_adoption_supported: support >= ProposalSupport::InboxAdoption,
            required_inbox_adoption: false,
            required_adoption: false,
            required_drafts: false,
            required_terminal: false,
            required_generation: false,
            required_retry: false,
            retry_supported: support >= ProposalSupport::Retry,
            public_adoption_supported: support >= ProposalSupport::PublicAdoption,
            required_public_adoption: false,
            suggestions_supported: support >= ProposalSupport::SuggestionsControl,
            required_suggestions: false,
            generation_supported: support >= ProposalSupport::Generation,
            brain_id: brain_id.to_owned(),
            root: File::from(open(
                &root_path,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?),
            root_path: root_path.clone(),
            state,
            journal_summaries: Mutex::default(),
            #[cfg(test)]
            journal_reads: std::sync::atomic::AtomicUsize::new(0),
            boundary,
        };
        let _lock = store.lock()?;
        let binding =
            serde_json::json!({"schema": SCHEMA, "brain_id": brain_id, "root": root_path});
        let path = store.state.join("binding.json");
        if path.exists() {
            let actual: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)
                .map_err(|_| error(ErrorCode::InvalidRequest, "invalid operational binding"))?;
            let mut base = actual.clone();
            let required = base
                .as_object_mut()
                .and_then(|v| v.remove("required_proposal_feed"));
            if let Some(required) = required {
                if support < ProposalSupport::Feed {
                    return Err(error(
                        ErrorCode::InvalidRequest,
                        "required proposal feed capability unsupported",
                    ));
                }
                let required: RequiredProposalFeed = serde_json::from_value(required)
                    .map_err(|_| error(ErrorCode::InvalidRequest, "invalid required binding"))?;
                required.validate()?;
                store.required_feed = Some(required);
            }
            if let Some(required) = base
                .as_object_mut()
                .and_then(|v| v.remove("required_proposal_drafts"))
            {
                if support < ProposalSupport::Drafts
                    || required != "tessera-proposal-drafts/v1"
                    || store.required_feed.is_none()
                {
                    return Err(error(
                        ErrorCode::InvalidRequest,
                        "required proposal drafts capability unsupported",
                    ));
                }
                store.required_drafts = true;
            }
            if let Some(required) = base
                .as_object_mut()
                .and_then(|v| v.remove("required_proposal_adoption"))
            {
                if support < ProposalSupport::Adoption
                    || required != "tessera-proposal-adoption/v1"
                    || !store.required_drafts
                {
                    return Err(error(
                        ErrorCode::InvalidRequest,
                        "required proposal adoption capability unsupported",
                    ));
                }
                store.required_adoption = true;
            }
            if let Some(required) = base
                .as_object_mut()
                .and_then(|v| v.remove("required_proposal_inbox_adoption"))
            {
                if support < ProposalSupport::InboxAdoption
                    || required != "tessera-proposal-inbox-adoption/v1"
                    || !store.required_adoption
                {
                    return Err(error(
                        ErrorCode::InvalidRequest,
                        "required Inbox adoption capability unsupported",
                    ));
                }
                store.required_inbox_adoption = true;
            }
            if let Some(required) = base
                .as_object_mut()
                .and_then(|v| v.remove("required_proposal_terminal"))
            {
                if support < ProposalSupport::Drafts
                    || !store.required_drafts
                    || required != "tessera-proposal-terminal/v1"
                {
                    return Err(error(
                        ErrorCode::InvalidRequest,
                        "required proposal terminal capability unsupported",
                    ));
                }
                store.required_terminal = true;
            }
            if let Some(required) = base
                .as_object_mut()
                .and_then(|v| v.remove("required_proposal_generation"))
            {
                if support < ProposalSupport::Generation
                    || !store.required_drafts
                    || required != "tessera-proposal-generation/v1"
                {
                    return Err(error(
                        ErrorCode::InvalidRequest,
                        "required proposal generation capability unsupported",
                    ));
                }
                store.required_generation = true;
            }
            if let Some(required) = base
                .as_object_mut()
                .and_then(|v| v.remove("required_proposal_retry"))
            {
                if support < ProposalSupport::Retry
                    || !store.required_generation
                    || required != "tessera-proposal-retry/v1"
                {
                    return Err(error(
                        ErrorCode::InvalidRequest,
                        "required proposal retry capability unsupported",
                    ));
                }
                store.required_retry = true;
            }
            if let Some(required) = base
                .as_object_mut()
                .and_then(|v| v.remove("required_public_proposal_adoption"))
            {
                if support < ProposalSupport::PublicAdoption
                    || !store.required_drafts
                    || required != "tessera-proposal-adopt/v1"
                {
                    return Err(error(
                        ErrorCode::InvalidRequest,
                        "required public proposal adoption capability unsupported",
                    ));
                }
                store.required_public_adoption = true;
            }
            if let Some(required) = base
                .as_object_mut()
                .and_then(|v| v.remove("required_suggestions_control"))
            {
                if support < ProposalSupport::SuggestionsControl
                    || required != "tessera-suggestions-control/v1"
                {
                    return Err(error(
                        ErrorCode::InvalidRequest,
                        "required suggestions control capability unsupported",
                    ));
                }
                store.required_suggestions = true;
            }
            if base != binding {
                return Err(error(
                    ErrorCode::InvalidRequest,
                    "operational directory belongs to another brain",
                ));
            }
            store.binding = actual;
        } else {
            store.persist("binding.json", &binding)?;
            store.binding = binding;
        }
        Ok(store)
    }

    pub fn required_suggestions_control(&self) -> bool {
        self.required_suggestions
    }

    /// Fence old opens and cached writers before any product activation/pause state.
    pub fn require_suggestions_control(&mut self) -> Result<()> {
        if !self.suggestions_supported || self.read_only || self.boundary != WriteBoundary::Managed
        {
            return Err(error(
                ErrorCode::InvalidRequest,
                "suggestions control requires a managed writer",
            ));
        }
        let _lock = self.lock()?;
        self.check_binding()?;
        if self.required_suggestions {
            return Ok(());
        }
        let mut binding = self.binding.clone();
        binding["required_suggestions_control"] = "tessera-suggestions-control/v1".into();
        self.persist("binding.json", &binding)?;
        self.binding = binding;
        self.required_suggestions = true;
        Ok(())
    }

    pub fn required_proposal_feed(&self) -> Option<&RequiredProposalFeed> {
        self.required_feed.as_ref()
    }

    /// Internal enrollment seam: preparing -> active only, never disable/relabel.
    /// An old binary with an already-open handle cannot be retroactively fenced;
    /// enrollment operationally requires draining every such handle first.
    pub fn require_proposal_feed(&mut self, next: RequiredProposalFeed) -> Result<()> {
        next.validate()?;
        if self.read_only || self.boundary != WriteBoundary::Managed {
            return Err(error(
                ErrorCode::InvalidRequest,
                "feed enrollment needs managed writer",
            ));
        }
        let _lock = self.lock()?;
        self.check_binding()?;
        match &self.required_feed {
            None if !next.active => (),
            Some(prior) if prior == &next => return Ok(()),
            Some(prior)
                if !prior.active
                    && next.active
                    && RequiredProposalFeed {
                        active: true,
                        ..prior.clone()
                    } == next => {}
            _ => {
                return Err(error(
                    ErrorCode::InvalidRequest,
                    "feed binding cannot be reset or relabeled",
                ))
            }
        }
        let mut binding = self.binding.clone();
        binding["required_proposal_feed"] = serde_json::to_value(&next)
            .map_err(|e| error(ErrorCode::InvalidRequest, e.to_string()))?;
        self.persist("binding.json", &binding)?;
        self.binding = binding;
        self.required_feed = Some(next);
        Ok(())
    }

    pub fn required_proposal_drafts(&self) -> bool {
        self.required_drafts
    }

    /// Fixture enrollment fences A2 fresh opens and existing upgraded handles.
    /// The retained A1 store is upgraded only after this durable source fence.
    pub fn require_proposal_drafts(&mut self) -> Result<()> {
        if !self.drafts_supported
            || self.read_only
            || self.boundary != WriteBoundary::Managed
            || !self.required_feed.as_ref().is_some_and(|b| b.active)
        {
            return Err(error(
                ErrorCode::InvalidRequest,
                "draft enrollment needs active managed feed",
            ));
        }
        let _lock = self.lock()?;
        self.check_binding()?;
        if self.required_drafts {
            return Ok(());
        }
        let mut binding = self.binding.clone();
        binding["required_proposal_drafts"] = "tessera-proposal-drafts/v1".into();
        self.persist("binding.json", &binding)?;
        self.binding = binding;
        self.required_drafts = true;
        Ok(())
    }

    pub fn required_proposal_adoption(&self) -> bool {
        self.required_adoption
    }
    pub fn require_proposal_adoption(&mut self) -> Result<()> {
        if !self.adoption_supported
            || self.read_only
            || self.boundary != WriteBoundary::Managed
            || !self.required_drafts
        {
            return Err(error(
                ErrorCode::InvalidRequest,
                "adoption enrollment needs managed drafts",
            ));
        }
        let _lock = self.lock()?;
        self.check_binding()?;
        if self.required_adoption {
            return Ok(());
        }
        let mut binding = self.binding.clone();
        binding["required_proposal_adoption"] = "tessera-proposal-adoption/v1".into();
        self.persist("binding.json", &binding)?;
        self.binding = binding;
        self.required_adoption = true;
        Ok(())
    }

    pub fn required_proposal_inbox_adoption(&self) -> bool {
        self.required_inbox_adoption
    }
    pub fn require_proposal_inbox_adoption(&mut self) -> Result<()> {
        if !self.inbox_adoption_supported
            || self.read_only
            || self.boundary != WriteBoundary::Managed
            || !self.required_adoption
        {
            return Err(error(
                ErrorCode::InvalidRequest,
                "Inbox adoption enrollment needs managed adoption",
            ));
        }
        let _lock = self.lock()?;
        self.check_binding()?;
        if self.required_inbox_adoption {
            return Ok(());
        }
        let mut binding = self.binding.clone();
        binding["required_proposal_inbox_adoption"] = "tessera-proposal-inbox-adoption/v1".into();
        self.persist("binding.json", &binding)?;
        self.binding = binding;
        self.required_inbox_adoption = true;
        Ok(())
    }

    pub fn required_proposal_terminal(&self) -> bool {
        self.required_terminal
    }

    /// Reserve terminal identities only after this durable capability fence.
    pub fn require_proposal_terminal(&mut self) -> Result<()> {
        if !self.required_drafts || self.read_only || self.boundary != WriteBoundary::Managed {
            return Err(error(
                ErrorCode::InvalidRequest,
                "terminal enrollment needs managed proposal drafts",
            ));
        }
        let _lock = self.lock()?;
        self.check_binding()?;
        if self.required_terminal {
            return Ok(());
        }
        let mut binding = self.binding.clone();
        binding["required_proposal_terminal"] = "tessera-proposal-terminal/v1".into();
        self.persist("binding.json", &binding)?;
        self.binding = binding;
        self.required_terminal = true;
        Ok(())
    }

    pub fn required_proposal_generation(&self) -> bool {
        self.required_generation
    }

    pub fn require_proposal_generation(&mut self) -> Result<()> {
        if !self.generation_supported
            || !self.required_drafts
            || self.read_only
            || self.boundary != WriteBoundary::Managed
        {
            return Err(error(
                ErrorCode::InvalidRequest,
                "generation enrollment needs managed proposal drafts",
            ));
        }
        let _lock = self.lock()?;
        self.check_binding()?;
        if self.required_generation {
            return Ok(());
        }
        let mut binding = self.binding.clone();
        binding["required_proposal_generation"] = "tessera-proposal-generation/v1".into();
        self.persist("binding.json", &binding)?;
        self.binding = binding;
        self.required_generation = true;
        Ok(())
    }

    pub fn required_proposal_retry(&self) -> bool {
        self.required_retry
    }
    pub fn require_proposal_retry(&mut self) -> Result<()> {
        if !self.retry_supported
            || !self.required_generation
            || self.read_only
            || self.boundary != WriteBoundary::Managed
        {
            return Err(error(
                ErrorCode::InvalidRequest,
                "retry requires managed generation",
            ));
        }
        let _lock = self.lock()?;
        self.check_binding()?;
        if self.required_retry {
            return Ok(());
        }
        let mut binding = self.binding.clone();
        binding["required_proposal_retry"] = "tessera-proposal-retry/v1".into();
        self.persist("binding.json", &binding)?;
        self.binding = binding;
        self.required_retry = true;
        Ok(())
    }

    pub fn require_public_proposal_adoption(&mut self) -> Result<()> {
        if !self.public_adoption_supported
            || !self.required_drafts
            || self.read_only
            || self.boundary != WriteBoundary::Managed
        {
            return Err(error(
                ErrorCode::InvalidRequest,
                "public adoption needs managed proposal drafts",
            ));
        }
        let _lock = self.lock()?;
        self.check_binding()?;
        if self.required_public_adoption {
            return Ok(());
        }
        let mut binding = self.binding.clone();
        binding["required_public_proposal_adoption"] = "tessera-proposal-adopt/v1".into();
        self.persist("binding.json", &binding)?;
        self.binding = binding;
        self.required_public_adoption = true;
        Ok(())
    }

    fn check_binding(&self) -> Result<()> {
        let actual: serde_json::Value =
            serde_json::from_slice(&fs::read(self.state.join("binding.json"))?)
                .map_err(|_| error(ErrorCode::InvalidRequest, "invalid operational binding"))?;
        if actual != self.binding {
            return Err(error(
                ErrorCode::InvalidRequest,
                "operational binding changed; reopen a supported handle",
            ));
        }
        Ok(())
    }

    pub(crate) fn lock(&self) -> Result<File> {
        let lock = File::from(open(
            self.state.join("writer.lock"),
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )?);
        lock.lock()?;
        Ok(lock)
    }

    // Directory descriptors + NOFOLLOW on every component prevent a symlink
    // swap from redirecting a lookup. Directory moves by unmanaged processes
    // during a write remain outside the Managed boundary.
    fn parent(&self, path: &str) -> Result<(File, String)> {
        if path.is_empty() || path.contains('\\') || path.contains('\0') || path.contains(':') {
            return Err(error(
                ErrorCode::InvalidPath,
                "expected a relative note path",
            ));
        }
        let mut parts: Vec<_> = path.split('/').collect();
        if parts
            .iter()
            .any(|p| p.is_empty() || *p == "." || *p == "..")
        {
            return Err(error(ErrorCode::InvalidPath, "invalid path component"));
        }
        let name = parts.pop().unwrap().to_owned();
        let mut parent = self.root.try_clone()?;
        for p in parts {
            parent = File::from(
                openat(
                    &parent,
                    p,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(path_error)?,
            );
        }
        Ok((parent, name))
    }

    fn bytes_at(parent: &File, name: &str) -> Result<Option<Vec<u8>>> {
        Ok(Self::source_at(parent, name)?.map(|(bytes, _)| bytes))
    }

    fn source_at(parent: &File, name: &str) -> Result<Option<(Vec<u8>, u32)>> {
        let fd = match openat(
            parent,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(e) => return Err(path_error(e)),
        };
        let mut file = File::from(fd);
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(error(
                ErrorCode::InvalidPath,
                "source must be a regular file",
            ));
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(Some((bytes, metadata.permissions().mode() & 0o777)))
    }

    pub fn read(&self, path: &str) -> Result<SourceSnapshot> {
        let _lock = self.lock()?;
        let (parent, name) = self.parent(path)?;
        let bytes = Self::bytes_at(&parent, &name)?
            .ok_or_else(|| error(ErrorCode::NotFound, "source does not exist"))?;
        Ok(SourceSnapshot {
            schema: SCHEMA.into(),
            brain_id: self.brain_id.clone(),
            path: path.into(),
            revision: revision(&bytes),
            content_base64: STANDARD.encode(&bytes),
            media_type: "text/markdown".into(),
        })
    }

    /// Root-bounded read for transports with an explicit byte budget. The
    /// descriptor is opened with the same no-symlink rules as exact source reads;
    /// the limit applies while reading, including if the file grows concurrently.
    pub fn read_bounded(&self, path: &str, max_bytes: u64) -> Result<SourceSnapshot> {
        let _lock = self.lock()?;
        let (parent, name) = self.parent(path)?;
        let fd = openat(
            &parent,
            name.as_str(),
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(path_error)?;
        let file = File::from(fd);
        if !file.metadata()?.is_file() {
            return Err(error(
                ErrorCode::InvalidPath,
                "source must be a regular file",
            ));
        }
        let mut bytes = Vec::new();
        file.take(max_bytes.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > max_bytes {
            return Err(error(
                ErrorCode::InvalidRequest,
                "source exceeds preview byte budget",
            ));
        }
        Ok(SourceSnapshot {
            schema: SCHEMA.into(),
            brain_id: self.brain_id.clone(),
            path: path.into(),
            revision: revision(&bytes),
            content_base64: STANDARD.encode(&bytes),
            media_type: "text/markdown".into(),
        })
    }

    pub fn write(&self, request: SourceWrite) -> Result<WriteReceipt> {
        self.write_with_base(request, None)
    }

    pub fn write_with_base(
        &self,
        request: SourceWrite,
        base: Option<SourceSnapshot>,
    ) -> Result<WriteReceipt> {
        self.write_base_with_hook(request, base, |_| Ok(()))
    }

    /// Read only the requested brain/path conflict. No write or retry occurs.
    pub fn conflict(&self, brain_id: &str, path: &str, id: &str) -> Result<ConflictView> {
        validate_id(id)?;
        let _lock = self.lock()?;
        let record = self
            .load(id)?
            .ok_or_else(|| error(ErrorCode::NotFound, "conflict not found"))?;
        if brain_id != self.brain_id
            || record.request.brain_id != brain_id
            || record.request.path != path
            || record.request.operation_id != id
        {
            return Err(error(
                ErrorCode::InvalidRequest,
                "conflict identity mismatch",
            ));
        }
        let conflict = record
            .conflict
            .ok_or_else(|| error(ErrorCode::InvalidRequest, "operation is not a conflict"))?;
        if conflict.conflict_id != id || conflict.path != path {
            return Err(error(
                ErrorCode::InvalidRequest,
                "stored conflict identity mismatch",
            ));
        }
        let (parent, name) = self.parent(path)?;
        let snapshot = |bytes: &[u8]| SourceSnapshot {
            schema: SCHEMA.into(),
            brain_id: brain_id.into(),
            path: path.into(),
            revision: revision(bytes),
            content_base64: STANDARD.encode(bytes),
            media_type: "text/markdown".into(),
        };
        let current = Self::bytes_at(&parent, &name)?.map(|bytes| snapshot(&bytes));
        let proposed = STANDARD
            .decode(&record.request.content_base64)
            .map_err(|_| error(ErrorCode::Indeterminate, "invalid preserved proposal"))?;
        Ok(ConflictView {
            conflict,
            base: record.base,
            current,
            proposed: snapshot(&proposed),
        })
    }

    /// Inspect recoverable bytes without changing or retrying the operation.
    pub fn recovery_record(&self, operation_id: &str) -> Result<RecoveryRecord> {
        validate_id(operation_id)?;
        let _lock = self.lock()?;
        self.load(operation_id)?
            .ok_or_else(|| error(ErrorCode::NotFound, "operation not found"))
    }

    /// Inspect only the requested journal with a caller-owned serialized-byte
    /// budget. Limit the actual read, including growth after opening the file;
    /// an oversized record is never treated as missing or partially decoded.
    /// This does not change legacy recovery reads or retry any canonical write.
    pub fn recovery_record_bounded(
        &self,
        operation_id: &str,
        max_bytes: u64,
    ) -> Result<RecoveryRecord> {
        validate_id(operation_id)?;
        let limit = max_bytes
            .checked_add(1)
            .filter(|_| max_bytes > 0)
            .ok_or_else(|| error(ErrorCode::InvalidRequest, "invalid recovery byte budget"))?;
        let _lock = self.lock()?;
        let file = match File::open(self.state.join(format!("{operation_id}.json"))) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(error(ErrorCode::NotFound, "operation not found"));
            }
            Err(e) => return Err(e.into()),
        };
        let mut bytes = Vec::new();
        file.take(limit).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > max_bytes {
            return Err(error(
                ErrorCode::InvalidRequest,
                "operation journal exceeds recovery byte budget",
            ));
        }
        #[cfg(test)]
        self.journal_reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        serde_json::from_slice(&bytes).map_err(|_| {
            error(
                ErrorCode::Indeterminate,
                "invalid operation journal; preserve for recovery",
            )
        })
    }

    fn load(&self, id: &str) -> Result<Option<RecoveryRecord>> {
        match fs::read(self.state.join(format!("{id}.json"))) {
            Ok(bytes) => {
                #[cfg(test)]
                self.journal_reads
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                serde_json::from_slice(&bytes).map(Some).map_err(|_| {
                    error(
                        ErrorCode::Indeterminate,
                        "invalid operation journal; preserve for recovery",
                    )
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    // Called only while writer.lock is held. Inspect metadata even on cache hits:
    // another SourceStore/process can publish or reconcile an intent between writes.
    fn pending_path(&self, id: &str) -> Result<Option<String>> {
        let identity = JournalIdentity::read(&self.state.join(format!("{id}.json")))?;
        let mut summaries = self.journal_summaries.lock().map_err(|_| {
            error(
                ErrorCode::Indeterminate,
                "journal summary cache unavailable",
            )
        })?;
        if let Some(summary) = summaries.get(id) {
            if summary.identity == identity {
                return Ok(summary.pending_path.clone());
            }
        }
        let record = self.load(id)?.ok_or_else(|| {
            error(
                ErrorCode::Indeterminate,
                "operation disappeared during inspection",
            )
        })?;
        let pending_path =
            (record.receipt.is_none() && record.conflict.is_none()).then_some(record.request.path);
        summaries.insert(
            id.into(),
            JournalSummary {
                identity,
                pending_path: pending_path.clone(),
            },
        );
        Ok(pending_path)
    }

    fn persist(&self, name: &str, value: &impl Serialize) -> Result<()> {
        let bytes =
            serde_json::to_vec(value).map_err(|e| error(ErrorCode::IoError, e.to_string()))?;
        let mut temp = tempfile::NamedTempFile::new_in(&self.state)?;
        temp.write_all(&bytes)?;
        temp.as_file().sync_all()?;
        temp.persist(self.state.join(name))
            .map_err(|e| SourceError::from(e.error))?;
        File::open(&self.state)?.sync_all()?;
        Ok(())
    }

    fn save(&self, record: &RecoveryRecord) -> Result<()> {
        self.persist(&format!("{}.json", record.request.operation_id), record)
    }

    #[cfg(test)]
    fn write_with_hook(
        &self,
        request: SourceWrite,
        hook: impl FnMut(CommitPoint) -> Result<()>,
    ) -> Result<WriteReceipt> {
        self.write_base_with_hook(request, None, hook)
    }

    fn write_base_with_hook(
        &self,
        request: SourceWrite,
        base: Option<SourceSnapshot>,
        mut hook: impl FnMut(CommitPoint) -> Result<()>,
    ) -> Result<WriteReceipt> {
        validate_id(&request.operation_id)?;
        if request.schema != SCHEMA || request.brain_id != self.brain_id {
            return Err(error(ErrorCode::InvalidRequest, "schema/brain mismatch"));
        }
        if let Some(r) = &request.expected_revision {
            if r.len() != 71
                || !r.starts_with("sha256:")
                || !r[7..]
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            {
                return Err(error(ErrorCode::InvalidRequest, "invalid source revision"));
            }
        }
        if let Some(base) = &base {
            let bytes = STANDARD
                .decode(&base.content_base64)
                .map_err(|_| error(ErrorCode::InvalidRequest, "invalid base bytes"))?;
            if base.schema != SCHEMA
                || base.brain_id != request.brain_id
                || base.path != request.path
                || request.expected_revision.as_ref() != Some(&base.revision)
                || revision(&bytes) != base.revision
            {
                return Err(error(
                    ErrorCode::InvalidRequest,
                    "base does not match the expected source revision",
                ));
            }
        }
        let proposed = STANDARD
            .decode(&request.content_base64)
            .map_err(|_| error(ErrorCode::InvalidRequest, "invalid source base64"))?;
        let _lock = self.lock()?;
        self.check_binding()?;
        if self.read_only || self.required_feed.as_ref().is_some_and(|f| !f.active) {
            return Err(error(
                ErrorCode::InvalidRequest,
                "read-only handle or feed enrollment incomplete",
            ));
        }
        let prior = self.load(&request.operation_id)?;
        if let Some(prior) = &prior {
            if prior.request != request || prior.base != base {
                return Err(error(
                    ErrorCode::InvalidRequest,
                    "operation ID reused with different input",
                ));
            }
            if let Some(receipt) = &prior.receipt {
                return Ok(receipt.clone());
            }
            if let Some(conflict) = &prior.conflict {
                return Err(conflict_error(conflict.clone()));
            }
        }
        // No newer cooperating operation may pass an unresolved intent on the
        // same path: otherwise content could change A→B→A and an old retry
        // would mistake A for evidence its B replacement never happened.
        for entry in fs::read_dir(&self.state)? {
            let path = entry?.path();
            let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if path.extension().and_then(|s| s.to_str()) != Some("json")
                || Uuid::parse_str(id).is_err()
                || id == request.operation_id
            {
                continue;
            }
            if self.pending_path(id)?.as_deref() == Some(request.path.as_str()) {
                return Err(error(
                    ErrorCode::Indeterminate,
                    format!("path blocked by pending operation {id}; reconcile it first"),
                ));
            }
        }
        let (parent, name) = self.parent(&request.path)?;
        let observed = Self::source_at(&parent, &name)?;
        let mode = observed.as_ref().map(|(_, mode)| *mode).unwrap_or(0o600);
        let current = observed.map(|(bytes, _)| bytes);
        let current_revision = current.as_deref().map(revision);
        let wanted_revision = revision(&proposed);
        let mut record = if let Some(mut prior) = prior {
            // A committed replacement whose acknowledgement was lost can be
            // recognized by its content. A divergent version is never replaced.
            if current_revision.as_ref() == Some(&wanted_revision) {
                parent.sync_all()?;
                let receipt = receipt(&prior, &wanted_revision);
                prior.receipt = Some(receipt.clone());
                self.save(&prior).map_err(indeterminate)?;
                return Ok(receipt);
            }
            if current_revision != prior.previous_revision {
                prior
                    .divergent_observations_base64
                    .push(current.as_deref().map(|b| STANDARD.encode(b)));
                self.save(&prior).map_err(indeterminate)?;
                return Err(error(
                    ErrorCode::Indeterminate,
                    "pending operation diverged; recover manually without overwriting",
                ));
            }
            prior
        } else {
            RecoveryRecord {
                base,
                request: request.clone(),
                preimage_base64: current.as_deref().map(|b| STANDARD.encode(b)),
                previous_revision: current_revision.clone(),
                receipt: None,
                conflict: None,
                divergent_observations_base64: Vec::new(),
            }
        };
        if self.boundary != WriteBoundary::Managed || request.expected_revision != current_revision
        {
            let conflict = SourceConflict {
                conflict_id: request.operation_id.clone(),
                path: request.path.clone(),
                expected_revision: request.expected_revision.clone(),
                current_revision,
                reason: if self.boundary == WriteBoundary::Managed {
                    "stale_revision"
                } else {
                    "unmanaged_writers"
                }
                .into(),
            };
            record.conflict = Some(conflict.clone());
            self.save(&record)?;
            return Err(conflict_error(conflict));
        }
        self.save(&record)?; // intent + preimage durable before source mutation
        hook(CommitPoint::IntentSaved)?;
        if current.as_deref() != Some(proposed.as_slice()) {
            let temporary = format!(".tessera-source-{}.tmp", Uuid::new_v4());
            // EXCL establishes ownership. Never unlink an entry we did not create,
            // including a canonical source that resembles a staging filename.
            let mut file = File::from(openat(
                &parent,
                temporary.as_str(),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )?);
            let preparation = (|| -> Result<()> {
                file.set_permissions(fs::Permissions::from_mode(mode))?;
                file.write_all(&proposed)?;
                file.sync_all()?;
                hook(CommitPoint::Prepared)?;
                Ok(())
            })();
            if let Err(e) = preparation {
                let _ =
                    rustix::fs::unlinkat(&parent, temporary.as_str(), rustix::fs::AtFlags::empty());
                return Err(e);
            }
            renameat(&parent, temporary.as_str(), &parent, name.as_str())
                .map_err(indeterminate_errno)?;
            parent.sync_all().map_err(|e| indeterminate(e.into()))?;
            hook(CommitPoint::Replaced).map_err(indeterminate)?;
        }
        let receipt = receipt(&record, &wanted_revision);
        record.receipt = Some(receipt.clone());
        self.save(&record).map_err(indeterminate)?;
        Ok(receipt)
    }
}

fn validate_id(id: &str) -> Result<()> {
    let parsed = Uuid::parse_str(id)
        .map_err(|_| error(ErrorCode::InvalidRequest, "invalid operation UUID"))?;
    if parsed.to_string() != id {
        return Err(error(
            ErrorCode::InvalidRequest,
            "operation UUID must be canonical",
        ));
    }
    Ok(())
}
fn receipt(record: &RecoveryRecord, new_revision: &str) -> WriteReceipt {
    WriteReceipt {
        operation_id: record.request.operation_id.clone(),
        path: record.request.path.clone(),
        previous_revision: record.previous_revision.clone(),
        revision: new_revision.into(),
        outcome: if record.previous_revision.as_deref() == Some(new_revision) {
            WriteOutcome::Unchanged
        } else {
            WriteOutcome::Written
        },
    }
}
fn conflict_error(conflict: SourceConflict) -> SourceError {
    SourceError {
        code: ErrorCode::Conflict,
        message: conflict.reason.clone(),
        conflict: Some(Box::new(conflict)),
    }
}
fn path_error(e: rustix::io::Errno) -> SourceError {
    match e {
        rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR => error(
            ErrorCode::InvalidPath,
            "symlink or non-directory in source path",
        ),
        rustix::io::Errno::NOENT => error(ErrorCode::NotFound, "source parent does not exist"),
        e => e.into(),
    }
}
fn indeterminate(e: SourceError) -> SourceError {
    error(ErrorCode::Indeterminate, e.message)
}
fn indeterminate_errno(e: rustix::io::Errno) -> SourceError {
    indeterminate(e.into())
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum CommitPoint {
    IntentSaved,
    Prepared,
    Replaced,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (tempfile::TempDir, SourceStore) {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("brain")).unwrap();
        fs::create_dir(temp.path().join("runtime")).unwrap();
        let store = SourceStore::open(
            "01000000-0000-4000-8000-000000000001",
            &temp.path().join("brain"),
            &temp.path().join("runtime"),
            WriteBoundary::Managed,
        )
        .unwrap();
        (temp, store)
    }
    fn request(expected: Option<String>) -> SourceWrite {
        SourceWrite {
            schema: SCHEMA.into(),
            operation_id: "06000000-0000-4000-8000-000000000001".into(),
            brain_id: "01000000-0000-4000-8000-000000000001".into(),
            path: "note.md".into(),
            expected_revision: expected,
            content_base64: STANDARD.encode(b"replacement"),
        }
    }
    #[test]
    fn bounded_recovery_reads_only_requested_record_at_exact_byte_boundary() {
        let (_temp, store) = setup();
        let request = request(None);
        let receipt = store.write(request.clone()).unwrap();
        let path = store.state.join(format!("{}.json", request.operation_id));
        let bytes = fs::read(&path).unwrap();
        // An unrelated corrupt journal must not turn this direct lookup into a scan.
        fs::write(
            store.state.join(format!("{}.json", Uuid::new_v4())),
            b"broken",
        )
        .unwrap();
        assert_eq!(
            store
                .recovery_record_bounded(&request.operation_id, bytes.len() as u64)
                .unwrap()
                .receipt,
            Some(receipt)
        );
        assert!(store
            .recovery_record_bounded(&request.operation_id, bytes.len() as u64 - 1)
            .unwrap_err()
            .message
            .contains("byte budget"));
        let mut extended = bytes.clone();
        extended.push(b' '); // Still valid JSON, but one byte over the frozen budget.
        fs::write(&path, &extended).unwrap();
        assert!(store
            .recovery_record_bounded(&request.operation_id, bytes.len() as u64)
            .unwrap_err()
            .message
            .contains("byte budget"));
        assert_eq!(fs::read(&path).unwrap(), extended);
        fs::write(&path, b"broken").unwrap();
        assert_eq!(
            store
                .recovery_record_bounded(&request.operation_id, 1024)
                .unwrap_err()
                .code,
            ErrorCode::Indeterminate
        );
        assert_eq!(
            store
                .recovery_record_bounded(&Uuid::new_v4().to_string(), 1024)
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
        assert_eq!(
            store
                .recovery_record_bounded("../invalid", 1024)
                .unwrap_err()
                .code,
            ErrorCode::InvalidRequest
        );
    }
    #[test]
    fn caller_uuid_and_proposed_bytes_survive_each_real_source_commit_fault() {
        for phase in [
            CommitPoint::IntentSaved,
            CommitPoint::Prepared,
            CommitPoint::Replaced,
        ] {
            let (temp, store) = setup();
            fs::write(temp.path().join("brain/note.md"), b"original").unwrap();
            let original = store.read("note.md").unwrap();
            let request = request(Some(original.revision.clone()));
            store
                .write_with_hook(request.clone(), |point| {
                    if point == phase {
                        return Err(error(ErrorCode::Indeterminate, "injected boundary failure"));
                    }
                    Ok(())
                })
                .unwrap_err();
            let path = store.state.join(format!("{}.json", request.operation_id));
            let before = fs::read(&path).unwrap();
            let retained = store
                .recovery_record_bounded(&request.operation_id, before.len() as u64)
                .unwrap();
            assert_eq!(retained.request, request);
            assert_eq!(
                retained.preimage_base64.as_deref(),
                Some(original.content_base64.as_str())
            );
            assert!(retained.receipt.is_none() && retained.conflict.is_none());
            assert_eq!(
                fs::read(&path).unwrap(),
                before,
                "bounded lookup must not reconcile the write"
            );
            assert_eq!(
                fs::read(temp.path().join("brain/note.md")).unwrap(),
                if phase == CommitPoint::Replaced {
                    b"replacement".to_vec()
                } else {
                    b"original".to_vec()
                }
            );
        }
    }
    #[test]
    fn required_feed_fences_open_handles_before_cached_receipt_replay() {
        let (temp, stale) = setup();
        let original = request(None);
        stale.write(original.clone()).unwrap();
        let mut supported = SourceStore::open_with_proposal_feed(
            &stale.brain_id,
            &temp.path().join("brain"),
            &temp.path().join("runtime"),
            WriteBoundary::Managed,
        )
        .unwrap();
        let mut binding = RequiredProposalFeed {
            capability: "tessera-proposal-feed/v1".into(),
            epoch: Uuid::new_v4().to_string(),
            activation_id: Uuid::new_v4().to_string(),
            policy_version: 1,
            active: false,
        };
        supported.require_proposal_feed(binding.clone()).unwrap();
        assert!(stale
            .write(original.clone())
            .unwrap_err()
            .message
            .contains("binding changed"));
        assert!(supported.write(original.clone()).is_err());
        assert!(SourceStore::open(
            &stale.brain_id,
            &temp.path().join("brain"),
            &temp.path().join("runtime"),
            WriteBoundary::Managed
        )
        .is_err());
        binding.active = true;
        supported.require_proposal_feed(binding.clone()).unwrap();
        supported.write(original.clone()).unwrap();
        let reader = SourceStore::open_read_only(
            &stale.brain_id,
            &temp.path().join("brain"),
            &temp.path().join("runtime"),
        )
        .unwrap();
        assert_eq!(
            reader.read("note.md").unwrap().content_base64,
            original.content_base64
        );
        assert!(reader.write(original).is_err());
        let mut changed = binding.clone();
        changed.epoch = Uuid::new_v4().to_string();
        assert!(supported.require_proposal_feed(changed).is_err());
        changed = binding;
        changed.active = false;
        assert!(supported.require_proposal_feed(changed).is_err());
    }
    #[test]
    fn long_conversation_does_not_reread_unchanged_recovery_bodies() {
        let (temp, s) = setup();
        let mut expected = None;
        // Each completed checkpoint retains increasingly large history. The
        // previous implementation deserialized N*(N-1)/2 recovery bodies.
        for i in 0..128 {
            let mut next = request(expected);
            next.operation_id = Uuid::new_v4().to_string();
            next.content_base64 = STANDARD.encode("x".repeat((i + 1) * 128));
            expected = Some(s.write(next).unwrap().revision);
        }
        let reads = s.journal_reads.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(reads, 127, "only newly encountered history may be read");
        // The cache is disposable: a fresh store must inspect all old histories.
        let reopened = SourceStore::open(
            &s.brain_id,
            &temp.path().join("brain"),
            &temp.path().join("runtime"),
            WriteBoundary::Managed,
        )
        .unwrap();
        let mut next = request(expected);
        next.operation_id = Uuid::new_v4().to_string();
        reopened.write(next).unwrap();
        assert_eq!(
            reopened
                .journal_reads
                .load(std::sync::atomic::Ordering::Relaxed),
            128
        );
    }

    #[test]
    fn cached_history_observes_other_store_pending_intent_and_reconciliation() {
        let (temp, s) = setup();
        let mut original = request(None);
        original.operation_id = Uuid::new_v4().to_string();
        let first = s.write(original.clone()).unwrap();
        let mut unrelated = request(None);
        unrelated.operation_id = Uuid::new_v4().to_string();
        unrelated.path = "unrelated.md".into();
        s.write(unrelated).unwrap(); // warm completed-history cache
        let other = SourceStore::open(
            &s.brain_id,
            &temp.path().join("brain"),
            &temp.path().join("runtime"),
            WriteBoundary::Managed,
        )
        .unwrap();
        let mut pending = request(Some(first.revision.clone()));
        pending.operation_id = Uuid::new_v4().to_string();
        pending.content_base64 = STANDARD.encode("second");
        other
            .write_with_hook(pending.clone(), |point| {
                if point == CommitPoint::Replaced {
                    Err(error(ErrorCode::IoError, "crash"))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
        let mut newer = request(Some(revision(b"second")));
        newer.operation_id = Uuid::new_v4().to_string();
        assert_eq!(
            s.write(newer.clone()).unwrap_err().code,
            ErrorCode::Indeterminate
        );
        // The pending cache entry must invalidate when a separate writer records
        // the lost receipt. Retry protection and the old receipt remain intact.
        other.write(pending).unwrap();
        s.write(newer).unwrap();
        assert_eq!(s.write(original).unwrap(), first);
    }

    #[test]
    fn replaced_or_corrupt_cached_history_is_reinspected() {
        let (_temp, s) = setup();
        let original = request(None);
        s.write(original.clone()).unwrap();
        let mut next = request(None);
        next.operation_id = Uuid::new_v4().to_string();
        next.path = "other.md".into();
        s.write(next.clone()).unwrap();
        // Simulate an externally replaced operation record after cache warmup.
        let mut record = s.recovery_record(&original.operation_id).unwrap();
        record.receipt = None;
        s.save(&record).unwrap();
        let mut blocked = request(Some(revision(b"replacement")));
        blocked.operation_id = Uuid::new_v4().to_string();
        assert_eq!(
            s.write(blocked.clone()).unwrap_err().code,
            ErrorCode::Indeterminate
        );
        s.write(original.clone()).unwrap();
        s.write(blocked).unwrap();
        fs::write(
            s.state.join(format!("{}.json", original.operation_id)),
            b"invalid",
        )
        .unwrap();
        next.operation_id = Uuid::new_v4().to_string();
        next.path = "third.md".into();
        assert_eq!(s.write(next).unwrap_err().code, ErrorCode::Indeterminate);
    }

    #[test]
    fn failure_before_replace_retains_original_and_retry_recovers_intent() {
        for failure in [CommitPoint::IntentSaved, CommitPoint::Prepared] {
            let (temp, s) = setup();
            fs::write(temp.path().join("brain/note.md"), b"original").unwrap();
            let request = request(Some(revision(b"original")));
            let mut fired = false;
            let e = s
                .write_with_hook(request.clone(), |point| {
                    if point == failure {
                        fired = true;
                        Err(error(ErrorCode::IoError, "injected disk failure"))
                    } else {
                        Ok(())
                    }
                })
                .unwrap_err();
            assert!(fired);
            assert_eq!(e.code, ErrorCode::IoError);
            assert_eq!(
                fs::read(temp.path().join("brain/note.md")).unwrap(),
                b"original"
            );
            assert_eq!(
                s.recovery_record(&request.operation_id)
                    .unwrap()
                    .preimage_base64,
                Some(STANDARD.encode(b"original"))
            );
            s.write(request).unwrap();
            assert_eq!(
                fs::read(temp.path().join("brain/note.md")).unwrap(),
                b"replacement"
            );
        }
    }
    #[test]
    fn lost_receipt_after_replace_reconciles_without_second_replacement() {
        let (temp, s) = setup();
        fs::write(temp.path().join("brain/note.md"), b"original").unwrap();
        let request = request(Some(revision(b"original")));
        let e = s
            .write_with_hook(request.clone(), |p| {
                if p == CommitPoint::Replaced {
                    Err(error(ErrorCode::IoError, "simulated crash"))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::Indeterminate);
        assert_eq!(
            fs::read(temp.path().join("brain/note.md")).unwrap(),
            b"replacement"
        );
        assert!(s
            .recovery_record(&request.operation_id)
            .unwrap()
            .receipt
            .is_none());
        drop(s);
        let s = SourceStore::open(
            "01000000-0000-4000-8000-000000000001",
            &temp.path().join("brain"),
            &temp.path().join("runtime"),
            WriteBoundary::Managed,
        )
        .unwrap();
        let receipt = s
            .write_with_hook(request.clone(), |_| {
                panic!("reconciliation must not re-enter commit")
            })
            .unwrap();
        assert_eq!(receipt.outcome, WriteOutcome::Written);
        assert_eq!(s.write(request).unwrap(), receipt);
    }
    #[test]
    fn divergent_recovery_is_indeterminate_and_preserves_observation() {
        let (temp, s) = setup();
        fs::write(temp.path().join("brain/note.md"), b"original").unwrap();
        let request = request(Some(revision(b"original")));
        s.write_with_hook(request.clone(), |p| {
            if p == CommitPoint::Replaced {
                Err(error(ErrorCode::IoError, "simulated crash"))
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        fs::write(temp.path().join("brain/note.md"), b"external after crash").unwrap();
        assert_eq!(
            s.write(request.clone()).unwrap_err().code,
            ErrorCode::Indeterminate
        );
        let record = s.recovery_record(&request.operation_id).unwrap();
        assert_eq!(
            record.divergent_observations_base64,
            vec![Some(STANDARD.encode(b"external after crash"))]
        );
        assert_eq!(
            fs::read(temp.path().join("brain/note.md")).unwrap(),
            b"external after crash"
        );
    }
    #[test]
    fn pending_path_barrier_prevents_aba_from_newer_cooperating_write() {
        let (temp, s) = setup();
        fs::write(temp.path().join("brain/note.md"), b"original").unwrap();
        let old = request(Some(revision(b"original")));
        s.write_with_hook(old.clone(), |p| {
            if p == CommitPoint::Replaced {
                Err(error(ErrorCode::IoError, "crash before receipt"))
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        let mut newer = old.clone();
        newer.operation_id = "06000000-0000-4000-8000-000000000002".into();
        newer.expected_revision = Some(revision(b"replacement"));
        newer.content_base64 = STANDARD.encode(b"original");
        assert_eq!(
            s.write(newer.clone()).unwrap_err().code,
            ErrorCode::Indeterminate
        );
        assert_eq!(
            fs::read(temp.path().join("brain/note.md")).unwrap(),
            b"replacement"
        );
        // Resolve the older receipt before another write can return content to A.
        let receipt = s.write(old.clone()).unwrap();
        s.write(newer).unwrap();
        assert_eq!(s.write(old).unwrap(), receipt);
        assert_eq!(
            fs::read(temp.path().join("brain/note.md")).unwrap(),
            b"original"
        );
    }
    #[test]
    fn write_lock_is_held_across_commit_and_released_for_other_processes() {
        let (_temp, s) = setup();
        let probe = || {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "source::tests::lock_probe_process",
                    "--ignored",
                    "--nocapture",
                ])
                .env("SOURCE_LOCK_PROBE", s.state.join("writer.lock"))
                .output()
                .unwrap();
            assert!(output.status.success());
            String::from_utf8(output.stdout).unwrap()
        };
        assert!(probe().contains("LOCK_ACQUIRED")); // positive control
        let mut probed = false;
        s.write_with_hook(request(None), |point| {
            if point == CommitPoint::Prepared {
                assert!(probe().contains("LOCK_BLOCKED"));
                probed = true;
            }
            Ok(())
        })
        .unwrap();
        assert!(probed);
        assert!(probe().contains("LOCK_ACQUIRED"));
    }

    #[test]
    #[ignore = "subprocess lock probe"]
    fn lock_probe_process() {
        let file = File::open(std::env::var_os("SOURCE_LOCK_PROBE").unwrap()).unwrap();
        match file.try_lock() {
            Ok(()) => println!("LOCK_ACQUIRED"),
            Err(std::fs::TryLockError::WouldBlock) => println!("LOCK_BLOCKED"),
            Err(e) => panic!("unexpected lock error: {e}"),
        }
    }
}
