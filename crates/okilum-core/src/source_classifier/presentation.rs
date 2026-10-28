//! Restricted, in-cap same-root presentation API. No Reader selection or frame proof.
//!
//! Compute root/publication requests outside an editor's `compose`. A held publication
//! is immutable authority for its own exact snapshot, never proof of current layout.
use super::{decorations::Marker, structural as root, SourceReason, StyleSpan};
use crate::source_projection::{Active, Bias, MapError, Projection, Snapshot};
use root::publication as published;
use std::{ops::Range, sync::Arc};

/// Only the closed canonical LP configuration is admitted. Unknown options and
/// stateful callbacks must retain their original fresh observation-only path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Context {
    LivePreview,
    Unknown,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Affinity {
    #[default]
    Start,
    End,
}
/// Full directed editor input. Units are bytes except at `begin_utf16`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NativeActive {
    pub anchor: usize,
    pub head: usize,
    pub composition: Option<Range<usize>>,
    pub replacement: Option<Range<usize>>,
    pub affinity: Affinity,
}
impl NativeActive {
    fn reveal(&self) -> Active {
        let composition = match (&self.composition, &self.replacement) {
            (Some(a), Some(b)) => Some(a.start.min(b.start)..a.end.max(b.end)),
            (a, b) => a.clone().or_else(|| b.clone()),
        };
        Active {
            selection: Some(self.anchor.min(self.head)..self.anchor.max(self.head)),
            composition,
        }
    }
}

/// Deliberately opaque: parser/session internals are not part of the public API.
#[derive(Debug)]
pub struct Error(published::Error);
impl From<published::Error> for Error {
    fn from(value: published::Error) -> Self {
        Self(value)
    }
}
impl From<root::Error> for Error {
    fn from(value: root::Error) -> Self {
        Self(value.into())
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "presentation refused: {:?}", self.0)
    }
}
impl std::error::Error for Error {}

/// Identity of the immutable core view, distinct from any native epoch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicationId(published::PublicationId);

pub struct RootRequest(root::Ticket);
pub struct RootCompletion(root::Completed);
impl RootRequest {
    pub fn compute(self) -> Result<RootCompletion, Error> {
        Ok(RootCompletion(self.0.compute()?))
    }
}
pub struct Request {
    inner: published::Request,
    source: Snapshot,
    active: NativeActive,
}
pub struct Completion {
    inner: published::Completed,
    source: Snapshot,
    active: NativeActive,
}
impl Request {
    pub fn compute(self) -> Result<Completion, Error> {
        Ok(Completion {
            inner: self.inner.compute()?,
            source: self.source,
            active: self.active,
        })
    }
}

#[derive(Default)]
pub struct Session {
    roots: root::Session,
    publications: published::Session,
    accepted: Option<Publication>,
}
impl Session {
    pub fn begin_root(&mut self, source: Snapshot, context: Context) -> Result<RootRequest, Error> {
        self.accepted = None;
        self.publications.invalidate();
        if context != Context::LivePreview {
            // A refused unknown context must also revoke older pending roots.
            self.roots = root::Session::default();
            return Err(root::Error::UnknownContext.into());
        }
        Ok(RootRequest(
            self.roots.begin(source, root::ParseContext::LivePreview)?,
        ))
    }
    pub fn adopt_root(&mut self, completion: RootCompletion) -> Result<(), Error> {
        self.roots.adopt(completion.0)?;
        Ok(())
    }
    pub fn reject_partial_root(&self, request: &RootRequest) -> Result<(), Error> {
        self.roots.reject_partial(&request.0)?;
        Ok(())
    }
    pub fn begin(
        &mut self,
        source: &Snapshot,
        context: Context,
        active: NativeActive,
    ) -> Result<Request, Error> {
        self.begin_units(source, context, active, false)
    }
    pub fn begin_utf16(
        &mut self,
        source: &Snapshot,
        context: Context,
        active: NativeActive,
    ) -> Result<Request, Error> {
        self.begin_units(source, context, active, true)
    }
    fn begin_units(
        &mut self,
        source: &Snapshot,
        context: Context,
        mut active: NativeActive,
        utf16: bool,
    ) -> Result<Request, Error> {
        self.accepted = None;
        self.publications.invalidate();
        if context != Context::LivePreview {
            return Err(root::Error::UnknownContext.into());
        }
        let check = |range| published::checked_range(&self.roots, source, range, utf16);
        // Each original endpoint/range is checked BEFORE replacement/composition union.
        active.anchor = check(active.anchor..active.anchor)?.start;
        active.head = check(active.head..active.head)?.start;
        active.composition = active.composition.map(check).transpose()?;
        active.replacement = active.replacement.map(check).transpose()?;
        let inner = self.publications.begin(
            &self.roots,
            source,
            known(),
            published::ActiveInput::Source(active.reveal()),
        )?;
        Ok(Request {
            inner,
            source: source.clone(),
            active,
        })
    }
    pub fn reject_partial(&self, request: &Request) -> Result<(), Error> {
        self.publications
            .reject_partial(&self.roots, &request.inner)?;
        Ok(())
    }
    pub fn adopt(&mut self, completion: Completion) -> Result<Publication, Error> {
        let inner = self.publications.adopt(&self.roots, completion.inner)?;
        let publication = Publication {
            id: PublicationId(inner.identity()),
            inner,
            source: completion.source,
            active: completion.active,
        };
        self.accepted = Some(publication.clone());
        Ok(publication)
    }
    pub fn current(&self) -> Option<&Publication> {
        let current = self.publications.current(&self.roots)?;
        self.accepted
            .as_ref()
            .filter(|p| Arc::ptr_eq(current, &p.inner))
    }
}
fn known() -> root::ContextClaim {
    root::ContextClaim::Known(root::ParseContext::LivePreview)
}

#[derive(Clone)]
pub struct Publication {
    id: PublicationId,
    inner: Arc<published::Publication>,
    source: Snapshot,
    active: NativeActive,
}
impl Publication {
    pub fn identity(&self) -> PublicationId {
        self.id.clone()
    }
    pub fn active(&self) -> &NativeActive {
        &self.active
    }
    /// All immutable metadata shares this checked lease; nothing here authorizes
    /// installation into a newer editor or claims a painted frame.
    pub fn view<'a>(
        &'a self,
        source: &'a Snapshot,
        context: Context,
        active: &NativeActive,
        id: &PublicationId,
    ) -> Result<View<'a>, Error> {
        if context != Context::LivePreview {
            return Err(root::Error::UnknownContext.into());
        }
        if &self.active != active {
            return Err(published::Error::ActiveMismatch.into());
        }
        let inner_view = published::View {
            source,
            context: known(),
            active: self.inner.active(),
            identity: &id.0,
        };
        let projection = self.inner.projection(&inner_view)?;
        Ok(View {
            publication: self,
            source,
            projection,
            styles: self.inner.styles(&inner_view)?,
            decorations: self.inner.decorations(&inner_view)?,
            reasons: self.inner.reasons(&inner_view)?,
        })
    }
    pub fn source(&self) -> &Snapshot {
        &self.source
    }
}

/// Constructible only by a full identity/active/context check. Derived values
/// borrow that immutable publication; no raw parser/root authority is exported.
pub struct View<'a> {
    publication: &'a Publication,
    source: &'a Snapshot,
    projection: &'a Projection,
    styles: &'a [StyleSpan],
    decorations: &'a [Marker],
    reasons: &'a [SourceReason],
}
impl View<'_> {
    pub fn display(&self) -> &str {
        self.projection.display()
    }
    pub fn styles(&self) -> &[StyleSpan] {
        self.styles
    }
    pub fn decorations(&self) -> &[Marker] {
        self.decorations
    }
    pub fn reasons(&self) -> &[SourceReason] {
        self.reasons
    }
    pub fn source_to_display(&self, offset: usize) -> Result<usize, MapError> {
        self.projection.source_to_display(self.source, offset)
    }
    pub fn display_to_source(&self, offset: usize, bias: Bias) -> Result<usize, MapError> {
        self.projection.display_to_source(self.source, offset, bias)
    }
    pub fn scope_is_raw(&self, scope: Range<usize>) -> Result<bool, Error> {
        Ok(self.publication.inner.scope_is_raw(
            &published::View {
                source: self.source,
                context: known(),
                active: self.publication.inner.active(),
                identity: &self.publication.id.0,
            },
            scope,
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn work() -> (usize, usize, usize, usize) {
        (
            super::super::TEST_PARSE_CALLS.with(|v| v.get()),
            crate::source_projection::TEST_INDEX_BUILDS.with(|v| v.get()),
            super::super::retained::TEST_RETAINED_BUILDS.with(|v| v.get()),
            super::super::retained::TEST_REVEAL_PREPARES.with(|v| v.get()),
        )
    }
    #[test]
    fn public_calls_have_real_cold_reuse_reveal_and_failed_adoption_work_controls() {
        let source = Snapshot::new("note", 1, "# title\n\n[x][id]\n\n[id]: /a 'title'\n");
        let mut session = Session::default();
        let before = work();
        let request = session
            .begin_root(source.clone(), Context::LivePreview)
            .unwrap();
        assert_eq!(work(), before, "root compute is explicitly separate");
        session.adopt_root(request.compute().unwrap()).unwrap();
        assert_eq!(
            work().0 - before.0,
            1,
            "real canonical parse positive control"
        );
        let index = session.roots.current().unwrap().clone();
        let fuel = index
            .references(&source, known())
            .unwrap()
            .fuel(&source, known())
            .unwrap();
        assert!(fuel.used > 0, "lookup/fuel positive control");
        let input = NativeActive::default();
        let before = work();
        let request = session
            .begin(&source, Context::LivePreview, input.clone())
            .unwrap();
        assert_eq!(work(), before);
        let first = session.adopt(request.compute().unwrap()).unwrap();
        let cold = work();
        assert_eq!(cold.0, before.0);
        assert!(cold.1 > before.1);
        assert_eq!(cold.2 - before.2, 1);
        assert_eq!(cold.3 - before.3, 1);
        let before = work();
        let repeat = session
            .begin(&source, Context::LivePreview, input.clone())
            .unwrap();
        let same = session.adopt(repeat.compute().unwrap()).unwrap();
        assert_eq!(same.identity(), first.identity());
        let view = same
            .view(&source, Context::LivePreview, &input, &same.identity())
            .unwrap();
        for offset in 0..=source.source().len() {
            let _ = view.source_to_display(offset);
        }
        assert!(!view.styles().is_empty());
        assert_eq!(
            work(),
            before,
            "view and actual identical reuse do no derived work"
        );
        let changed = NativeActive {
            anchor: source.source().len(),
            head: source.source().len(),
            ..input.clone()
        };
        let next = session
            .begin(&source, Context::LivePreview, changed)
            .unwrap();
        let next = session.adopt(next.compute().unwrap()).unwrap();
        assert_ne!(next.identity(), first.identity());
        assert_eq!(work().0, before.0);
        assert_eq!(work().2, before.2);
        assert_eq!(work().3 - before.3, 1);
        let pending = session
            .begin(&source, Context::LivePreview, input.clone())
            .unwrap()
            .compute()
            .unwrap();
        let partial = session
            .begin(&source, Context::LivePreview, input.clone())
            .unwrap();
        let before = work();
        assert!(session.reject_partial(&partial).is_err());
        assert!(session.adopt(pending).is_err());
        let invalid = NativeActive {
            composition: Some(usize::MAX..usize::MAX),
            ..input.clone()
        };
        assert!(session
            .begin(&source, Context::LivePreview, invalid)
            .is_err());
        assert!(session.current().is_none());
        assert_eq!(work(), before);
        assert_eq!(
            index
                .references(&source, known())
                .unwrap()
                .fuel(&source, known())
                .unwrap(),
            fuel
        );
        let before = work();
        let mut second = Session::default();
        let root = second
            .begin_root(source.clone(), Context::LivePreview)
            .unwrap();
        second.adopt_root(root.compute().unwrap()).unwrap();
        let request = second.begin(&source, Context::LivePreview, input).unwrap();
        second.adopt(request.compute().unwrap()).unwrap();
        assert_eq!(work().0 - before.0, 1);
        assert_eq!(
            work().2 - before.2,
            1,
            "repeated cold action differs from a hit"
        );
    }
}
