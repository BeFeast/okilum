//! Batch preparation. Call off the UI thread with a source reader owned by the
//! caller's root authority. No URL requests and no persistent content cache.
use super::{destination, resolve, Destination, HeadingFailure, HeadingInventory, ResolvedLink};
use crate::Vault;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkStatus {
    Resolved,
    MissingDocument,
    MissingFile,
    MissingHeading,
    Ambiguous,
    Unsupported,
    Unknown,
    External,
}
impl LinkStatus {
    pub fn is_missing(self) -> bool {
        matches!(
            self,
            Self::MissingDocument | Self::MissingFile | Self::MissingHeading
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkState {
    pub status: LinkStatus,
    pub reason: String,
    pub target_revision: Option<String>,
    #[serde(default)]
    pub action_url: Option<String>,
}
impl LinkState {
    pub fn unknown() -> Self {
        Self::new(LinkStatus::Unknown, "Link unavailable.")
    }
    pub(super) fn new(status: LinkStatus, reason: impl Into<String>) -> Self {
        Self {
            status,
            reason: reason.into(),
            target_revision: None,
            action_url: None,
        }
    }
}

/// Authored interpretation paired with the exact URL emitted into a rendered
/// snapshot. Refresh never reconstructs this identity from a newer filesystem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkIdentity {
    pub from: String,
    pub target: String,
    pub wiki: bool,
    pub url: String,
}

/// One source snapshot. `headings` must describe the same rendering surface as
/// navigation. Managed callers can reject Setext without narrowing the Reader.
pub struct TargetSnapshot {
    pub revision: String,
    pub headings: HeadingInventory,
    pub supports_setext: bool,
    pub managed: Option<crate::source_classifier::Classification>,
}

/// Short-lived cache: one resolution per grammar/target, one read and heading
/// parse per unique candidate in this source/root/generation preparation job.
pub struct LinkPreparation<'a, F> {
    vault: &'a Vault,
    from: String,
    load: F,
    local_files: Option<Vault>,
    targets: BTreeMap<String, Result<TargetSnapshot, String>>,
    results: BTreeMap<(String, bool, String), (ResolvedLink, LinkState)>,
}
impl<'a, F: FnMut(&str) -> Result<TargetSnapshot, String>> LinkPreparation<'a, F> {
    pub fn new(vault: &'a Vault, from: &str, load: F) -> Self {
        Self {
            vault,
            from: from.to_owned(),
            load,
            local_files: None,
            targets: BTreeMap::new(),
            results: BTreeMap::new(),
        }
    }
    /// Desktop Reader only: filesystem checks run in the caller's background job.
    /// Managed previews retain their scoped source/asset authority by default.
    pub fn with_local_files(mut self) -> Self {
        let mut live = self.vault.clone();
        live.graph_root = None;
        self.local_files = Some(live);
        self
    }

    pub fn readable(&mut self, path: &str) -> bool {
        self.targets
            .entry(path.to_owned())
            .or_insert_with(|| (self.load)(path))
            .is_ok()
    }
    pub fn link(&mut self, target: &str, wiki: bool) -> (ResolvedLink, LinkState) {
        self.link_from(&self.from.clone(), target, wiki)
    }
    fn link_from(&mut self, from: &str, target: &str, wiki: bool) -> (ResolvedLink, LinkState) {
        let key = (from.to_owned(), wiki, target.to_owned());
        if let Some(result) = self.results.get(&key) {
            return result.clone();
        }
        let mut local_document = None;
        if let Some(live) = &self.local_files {
            if let Some(result) = super::local_files::prepare(live, from, target, wiki) {
                if result.0.status == "resolved" {
                    local_document = Some(result.0);
                } else {
                    self.results.insert(key, result.clone());
                    return result;
                }
            }
        }
        let resolved = local_document.unwrap_or_else(|| resolve(target, wiki, self.vault, from));
        let mut state = match resolved.status {
            "external" => LinkState::new(LinkStatus::External, "External link"),
            "unsupported" => LinkState::new(
                LinkStatus::Unsupported,
                resolved.reason.unwrap_or("Unsupported link"),
            ),
            "ambiguous" => LinkState::new(
                LinkStatus::Ambiguous,
                "Several documents match. Click to choose a destination.",
            ),
            "unresolved" if self.absence_is_known(from, target, wiki) => LinkState::new(
                LinkStatus::MissingDocument,
                format!("Document not found: {target}"),
            ),
            "outside_file" => {
                LinkState::new(LinkStatus::Resolved, "File outside vault: choose Reveal")
            }
            "attachment" => LinkState::new(LinkStatus::Resolved, "Preview file"),
            "resolved" => LinkState::new(LinkStatus::Resolved, "Open document"),
            _ => LinkState::unknown(),
        };
        if resolved.status == "resolved" {
            let path = &resolved.candidates[0];
            let snapshot = self
                .targets
                .entry(path.clone())
                .or_insert_with(|| (self.load)(path));
            match snapshot {
                Err(reason) => state = LinkState::new(LinkStatus::Unknown, reason.clone()),
                Ok(snapshot) => {
                    state.target_revision = Some(snapshot.revision.clone());
                    if let Some(heading) = &resolved.heading {
                        let result = snapshot.headings.locate(heading);
                        let (status, reason) = match result {
                            Ok(h) if h.setext && !snapshot.supports_setext => (
                                LinkStatus::Unsupported,
                                "This preview cannot navigate Setext headings.".into(),
                            ),
                            Ok(_) if heading.starts_with('^') => {
                                (LinkStatus::Resolved, format!("Open block: {heading}"))
                            }
                            Ok(_) => (LinkStatus::Resolved, format!("Open heading: {heading}")),
                            Err(HeadingFailure::Missing) => (
                                LinkStatus::MissingHeading,
                                format!("Heading not found: {heading}"),
                            ),
                            Err(HeadingFailure::MissingBlock) => (
                                LinkStatus::MissingHeading,
                                format!("Block not found: {heading}"),
                            ),
                            Err(HeadingFailure::AmbiguousBlock) => (
                                LinkStatus::Ambiguous,
                                HeadingFailure::AmbiguousBlock.reason().into(),
                            ),
                            Err(HeadingFailure::Ambiguous) => (
                                LinkStatus::Ambiguous,
                                HeadingFailure::Ambiguous.reason().into(),
                            ),
                            Err(HeadingFailure::Unsupported) => (
                                LinkStatus::Unsupported,
                                HeadingFailure::Unsupported.reason().into(),
                            ),
                        };
                        state.status = status;
                        state.reason = reason;
                        if let Some(managed) = &snapshot.managed {
                            if status == LinkStatus::Resolved
                                || managed.snapshot().source().len()
                                    > crate::source_classifier::MAX_BYTES
                            {
                                if let Err(reason) = managed
                                    .heading_offset_with_inventory(&snapshot.headings, heading)
                                {
                                    state.status = LinkStatus::Unsupported;
                                    state.reason = reason.into();
                                }
                            }
                        }
                    }
                }
            }
        }
        if !self.vault.paths_complete()
            && !matches!(state.status, LinkStatus::External | LinkStatus::Unsupported)
        {
            state = LinkState::unknown();
            state.reason = "Link unavailable while the vault is loading.".into();
        }
        state.action_url = Some(resolved.url.clone());
        self.results.insert(key, (resolved.clone(), state.clone()));
        (resolved, state)
    }

    fn absence_is_known(&self, from: &str, target: &str, wiki: bool) -> bool {
        if !self.vault.paths_complete() {
            return false;
        }
        let Destination::Note { path, .. } = destination(target, wiki) else {
            return false;
        };
        // Existing unreadable, dangling or invalid entries and root escapes are
        // unavailable, never proof of absence. Resolver still owns identity.
        if path.is_empty() {
            return false;
        }
        let relative = if path.starts_with('/') {
            path.trim_start_matches('/').to_owned()
        } else {
            Path::new(from)
                .parent()
                .unwrap_or(Path::new(""))
                .join(&path)
                .to_string_lossy()
                .into_owned()
        };
        let mut depth = 0usize;
        for part in Path::new(&relative).components() {
            match part {
                std::path::Component::ParentDir if depth == 0 => return false,
                std::path::Component::ParentDir => depth -= 1,
                std::path::Component::Normal(_) => depth += 1,
                std::path::Component::RootDir | std::path::Component::Prefix(_) => return false,
                _ => {}
            }
        }
        [relative, path.trim_start_matches('/').to_owned()].iter().all(|p| {
            let p = if wiki && !p.to_lowercase().ends_with(".md") { format!("{p}.md") } else { p.clone() };
            matches!(std::fs::symlink_metadata(self.vault.root.join(p)), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
        })
    }

    pub fn identities(&mut self, identities: &[LinkIdentity]) -> BTreeMap<String, LinkState> {
        let mut states = BTreeMap::new();
        for identity in identities {
            let (_, state) = self.link_from(&identity.from, &identity.target, identity.wiki);
            states
                .entry(identity.url.clone())
                .and_modify(|old| {
                    if old != &state {
                        *old = LinkState::unknown();
                    }
                })
                .or_insert(state);
        }
        states
    }

    pub fn source(&mut self, source: &str) -> BTreeMap<String, LinkState> {
        let mut states = BTreeMap::new();
        for link in super::parse_in_vault(source, self.vault, &self.from) {
            let (resolved, state) = self.link(&link.target, link.wiki);
            states
                .entry(resolved.url)
                .and_modify(|old| {
                    if old != &state {
                        *old = LinkState::unknown();
                    }
                })
                .or_insert(state);
        }
        states
    }
}
