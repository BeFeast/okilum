//! Two-document host and guarded ownership transitions (#773).
//! Parent integration supplies existing rendered surfaces and FileEditor handles.
//! The ownership model owns no watcher, cache, sidebar or persisted layout.
//! The concrete source adapter delegates IO to the existing FileEditor.
#![cfg_attr(not(test), allow(dead_code, reason = "#773, not wired yet"))]
use super::reader_document::{Document, Identity};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub(super) struct PaneId(u64);

impl PaneId {
    fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(
            NEXT.try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
                .expect("pane identity exhausted"),
        )
    }
}

/// Capture this for every document worker, including links and reconciliation.
/// A ticket cannot be constructed by callers or reused after publication/close.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct LoadTicket {
    pub pane: PaneId,
    generation: u64,
}

struct Pane<S, E> {
    id: PaneId,
    document: Document<S, E>,
    generation: u64,
    pending: Option<Identity>,
}

impl<S, E> Pane<S, E> {
    fn new(document: Document<S, E>) -> Self {
        Self {
            id: PaneId::next(),
            document,
            generation: 0,
            pending: None,
        }
    }

    fn invalidate(&mut self) {
        self.generation = self
            .generation
            .checked_add(1)
            .expect("pane generation exhausted");
        self.pending = None;
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum ExitError<G> {
    MissingPane,
    LastPane,
    Stale,
    Guard(G),
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum EditError<F> {
    MissingPane,
    /// The host has focused this owner. Show the existing concise notice.
    Owned(PaneId),
    /// Includes sibling-window FileEditor lock refusal; never steal ownership.
    Open(F),
}

pub(super) struct Panes<S, E> {
    panes: Vec<Pane<S, E>>,
    active: PaneId,
    recent: Vec<PaneId>,
}

impl<S, E> Panes<S, E> {
    pub(super) fn new(document: Document<S, E>) -> Self {
        let pane = Pane::new(document);
        let active = pane.id;
        Self {
            panes: vec![pane],
            active,
            recent: vec![active],
        }
    }

    pub(super) fn active(&self) -> PaneId {
        self.active
    }

    pub(super) fn ids(&self) -> impl Iterator<Item = PaneId> + '_ {
        self.panes.iter().map(|pane| pane.id)
    }

    pub(super) fn document(&self, id: PaneId) -> Option<&Document<S, E>> {
        self.panes
            .iter()
            .find(|pane| pane.id == id)
            .map(|pane| &pane.document)
    }

    pub(super) fn document_mut(&mut self, id: PaneId) -> Option<&mut Document<S, E>> {
        self.panes
            .iter_mut()
            .find(|pane| pane.id == id)
            .map(|pane| &mut pane.document)
    }

    /// Focus alone never drops/saves an editor or resets history/caret/scroll.
    pub(super) fn focus(&mut self, id: PaneId) -> bool {
        if self.document(id).is_none() {
            return false;
        }
        self.active = id;
        self.recent.retain(|existing| *existing != id);
        self.recent.insert(0, id);
        true
    }

    pub(super) fn previous(&self) -> Option<PaneId> {
        self.recent.iter().copied().find(|id| *id != self.active)
    }

    /// Only for a successfully prepared adjacent document. Until then keep the
    /// single-pane layout. When already split, load into the other pane instead.
    /// The original surface and editor stay in place without cloning either.
    pub(super) fn open_beside(
        &mut self,
        document: Document<S, E>,
    ) -> Result<PaneId, Document<S, E>> {
        if self.panes.len() == 2 {
            return Err(document);
        }
        let pane = Pane::new(document);
        let id = pane.id;
        self.panes.push(pane);
        self.focus(id);
        Ok(id)
    }

    pub(super) fn claim_editor<F>(
        &mut self,
        id: PaneId,
        open: impl FnOnce(&Identity) -> Result<E, F>,
    ) -> Result<(), EditError<F>> {
        let identity = self
            .document(id)
            .ok_or(EditError::MissingPane)?
            .identity()
            .clone();
        if let Some(owner) = self
            .panes
            .iter()
            .find(|pane| pane.document.identity() == &identity && pane.document.editor().is_some())
            .map(|pane| pane.id)
        {
            self.focus(owner);
            return Err(EditError::Owned(owner));
        }
        // FileEditor open still performs OS/source/recovery checks across windows.
        let editor = open(&identity).map_err(EditError::Open)?;
        self.document_mut(id)
            .expect("checked pane")
            .install_editor(editor);
        Ok(())
    }

    /// Preparation may run while the accepted document stays visible. Starting
    /// another load invalidates the first even when the paths are equal (A/B/A).
    pub(super) fn begin_load(&mut self, id: PaneId, target: Identity) -> Option<LoadTicket> {
        let pane = self.panes.iter_mut().find(|pane| pane.id == id)?;
        pane.invalidate();
        pane.pending = Some(target);
        Some(LoadTicket {
            pane: id,
            generation: pane.generation,
        })
    }

    pub(super) fn is_current(&self, ticket: LoadTicket) -> bool {
        self.panes.iter().any(|pane| {
            pane.id == ticket.pane && pane.generation == ticket.generation && pane.pending.is_some()
        })
    }

    /// Stamp post-publication link/recovery/reconciliation work separately from
    /// a pending load. Focus changes do not invalidate work for the inactive pane.
    pub(super) fn document_ticket(&self, id: PaneId) -> Option<LoadTicket> {
        let pane = self.panes.iter().find(|pane| pane.id == id)?;
        Some(LoadTicket {
            pane: id,
            generation: pane.generation,
        })
    }

    pub(super) fn accepts_document_work(&self, ticket: LoadTicket) -> bool {
        self.panes
            .iter()
            .any(|pane| pane.id == ticket.pane && pane.generation == ticket.generation)
    }

    /// Use on cancellation/failure as well, so late success cannot publish.
    pub(super) fn cancel_load(&mut self, ticket: LoadTicket) -> bool {
        if !self.is_current(ticket) {
            return false;
        }
        self.panes
            .iter_mut()
            .find(|pane| pane.id == ticket.pane)
            .unwrap()
            .invalidate();
        true
    }

    /// Call only after normal source/path/fragment validation. The guard runs at
    /// publication time: edits made while preparation was pending must also save.
    /// `apply` publishes the existing prepared payload into that pane's surface;
    /// it must retain that pane's navigation history and chosen mode.
    pub(super) fn complete_load<G>(
        &mut self,
        ticket: LoadTicket,
        guard: impl FnOnce(&mut Document<S, E>) -> Result<(), G>,
        apply: impl FnOnce(&mut S),
    ) -> Result<(), ExitError<G>> {
        if !self.is_current(ticket) {
            return Err(ExitError::Stale);
        }
        let pane = self
            .panes
            .iter_mut()
            .find(|pane| pane.id == ticket.pane)
            .unwrap();
        // Consume a refused completion too; retry needs fresh preparation.
        let target = pane.pending.take().unwrap();
        pane.invalidate();
        pane.document
            .leave_editor(guard)
            .map_err(ExitError::Guard)?;
        pane.document.accept(target, apply);
        Ok(())
    }

    /// Persist layout only after Ok. On refusal both panes, their editor handles,
    /// pending workers and active selection remain mounted.
    pub(super) fn close<G>(
        &mut self,
        id: PaneId,
        guard: impl FnOnce(&mut Document<S, E>) -> Result<(), G>,
    ) -> Result<Document<S, E>, ExitError<G>> {
        let index = self
            .panes
            .iter()
            .position(|pane| pane.id == id)
            .ok_or(ExitError::MissingPane)?;
        if self.panes.len() == 1 {
            return Err(ExitError::LastPane);
        }
        guard(&mut self.panes[index].document).map_err(ExitError::Guard)?;
        let removed = self.panes.remove(index);
        self.recent.retain(|existing| *existing != id);
        if self.active == id {
            self.active = self.recent[0];
        }
        Ok(removed.document)
    }

    /// Keep active; never first collapse the layout and then attempt the save.
    pub(super) fn unsplit<G>(
        &mut self,
        guard: impl FnOnce(&mut Document<S, E>) -> Result<(), G>,
    ) -> Result<Document<S, E>, ExitError<G>> {
        let other = self
            .panes
            .iter()
            .find(|pane| pane.id != self.active)
            .map(|pane| pane.id)
            .ok_or(ExitError::LastPane)?;
        self.close(other, guard)
    }

    /// Quit must attempt every pane even after an earlier failure. The existing
    /// quit guard decides conflict/durable recovery policy; this does not drop
    /// handles or claim that a queued recovery write is durable.
    pub(super) fn guard_all<G>(
        &mut self,
        mut guard: impl FnMut(PaneId, &mut Document<S, E>) -> Result<(), G>,
    ) -> Result<(), Vec<(PaneId, G)>> {
        let mut failures = Vec::new();
        for pane in &mut self.panes {
            if let Err(error) = guard(pane.id, &mut pane.document) {
                failures.push((pane.id, error));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures)
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(any(unix, windows))]
impl Panes<crate::prepared_links::PreparedDocument, okilum_core::file_editor::FileEditor> {
    /// FileEditor remains the authority across panes and windows. Its checked
    /// source open, revision baseline, recovery journal and advisory lock are
    /// retained in the owner; no rendered Markdown is used as editable source.
    pub(super) fn claim_source_editor(
        &mut self,
        id: PaneId,
        drafts: &std::path::Path,
    ) -> Result<(), EditError<anyhow::Error>> {
        self.claim_editor(id, |identity| {
            // Validate containment without erasing the authored path: FileEditor
            // must still see and refuse a symlink/hard-link source itself.
            crate::reader_files::checked_path(identity.root(), identity.relative())?;
            okilum_core::file_editor::FileEditor::open(
                &identity.root().join(identity.relative()),
                drafts,
            )
        })
    }
}
