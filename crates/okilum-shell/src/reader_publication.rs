//! Worker-verified immutable source publications, independent of the mutable
//! baseline (which may already belong to the next batch).
use super::*;
use sha2::{Digest, Sha256};

pub(super) struct Source {
    pub raw: Arc<str>,
    digest: [u8; 32],
    verified: bool,
    pub version: u64,
}
impl Source {
    pub fn new(raw: String, verified: bool, version: u64) -> Arc<Self> {
        Arc::new(Self {
            digest: Sha256::digest(raw.as_bytes()).into(),
            raw: raw.into(),
            verified,
            version,
        })
    }
}
pub(super) type Sources = std::collections::HashMap<String, Arc<Source>>;

pub(super) fn snapshot_sources(
    state: &okilum_core::vault::warm::incremental::State,
) -> Arc<Sources> {
    let unavailable: std::collections::HashSet<_> = state
        .vault
        .unreadable
        .iter()
        .map(|entry| entry.path.clone())
        .chain(state.vault.cloud_placeholders.iter().cloned())
        .collect();
    Arc::new(
        state
            .snapshot
            .markdown_source_paths()
            .filter_map(|path| {
                state.snapshot.source(path).map(|raw| {
                    (
                        path.to_owned(),
                        Source::new(raw, !unavailable.contains(&state.vault.root.join(path)), 0),
                    )
                })
            })
            .collect(),
    )
}

impl Reader {
    /// Only committed bytes verified by the worker authorize suppressing an
    /// echo. Paths, watcher hints and disk stamps alone never authorize it.
    pub(super) fn reconcile_source_publication(
        &mut self,
        source: Option<&Arc<Source>>,
        graph_changed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.current_rel.is_empty() {
            return;
        }
        if source.is_none() && !graph_changed {
            return;
        }
        if self.editing.is_some() {
            if source.is_some_and(|source| {
                source.verified && self.owns_source_publication(&source.digest)
            }) && self.deleted_current().is_none()
            {
                return;
            }
            self.refresh_source_in_background(window, cx);
        } else if self.has_open_note_watch() {
            // The mounted source owner may already have observed newer bytes
            // than this inventory publication. Do not roll its document back.
            self.reconcile_published_document(window, cx);
        } else if let Some(source) = source {
            self.reconcile_document_source(Some(source.raw.clone()), window, cx);
        } else {
            self.reconcile_published_document(window, cx);
        }
    }
}
