//! Current-root immutable presentation, not Reader/native frame acceptance.
use super::{BlockIndex, ContextClaim, ParseContext, Session as Roots};
use crate::{
    source_classifier::{RetainedPresentation, RevealSnapshot},
    source_projection::{Active, Bias, FallbackReason, MapError, Snapshot},
};
use std::{ops::Range, sync::Arc};

#[derive(Debug)]
struct Identity;

/// Identity of this owned immutable view, NOT a semantic/options identity token.
#[derive(Clone, Debug)]
pub(crate) struct PublicationId(Arc<Identity>);
impl PartialEq for PublicationId {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for PublicationId {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Error {
    Authority(super::Error),
    Projection(FallbackReason),
    Mapping(MapError),
    PublicationMismatch,
    ActiveMismatch,
    StaleRequest,
    Incomplete,
    SequenceLimit,
}
impl From<super::Error> for Error {
    fn from(e: super::Error) -> Self {
        Self::Authority(e)
    }
}

/// Units are explicit. Surrogate/grapheme/CRLF interiors are refused, not rounded.
pub(crate) enum ActiveInput {
    Source(Active),
    Utf16(Active),
}

pub(crate) struct View<'a> {
    pub source: &'a Snapshot,
    pub context: ContextClaim,
    pub active: &'a Active,
    pub identity: &'a PublicationId,
}

/// Validate independent native inputs against the existing root index, before union.
pub(crate) fn checked_range(
    roots: &Roots,
    source: &Snapshot,
    range: Range<usize>,
    utf16: bool,
) -> Result<Range<usize>, Error> {
    let root = Root::accepted(
        roots,
        source,
        ContextClaim::Known(ParseContext::LivePreview),
    )?;
    let active = Active {
        selection: Some(range),
        composition: None,
    };
    let input = if utf16 {
        ActiveInput::Utf16(active)
    } else {
        ActiveInput::Source(active)
    };
    root.active(input)?.selection.ok_or(Error::ActiveMismatch)
}

#[derive(Clone)]
struct Root {
    index: Arc<BlockIndex>,
    sequence: u64,
    source: Snapshot,
    context: ParseContext,
}
impl Root {
    fn accepted(roots: &Roots, source: &Snapshot, context: ContextClaim) -> Result<Self, Error> {
        let context = match context {
            ContextClaim::Known(c) => c,
            ContextClaim::Unknown => return Err(super::Error::UnknownContext.into()),
        };
        let index = roots.current().ok_or(Error::Incomplete)?;
        index.check(source, ContextClaim::Known(context))?;
        let ticket = roots.requested.as_ref().ok_or(Error::Incomplete)?;
        Ok(Self {
            index: index.clone(),
            sequence: ticket.sequence,
            source: source.clone(),
            context,
        })
    }
    fn current(&self, roots: &Roots) -> Result<(), Error> {
        let current = Self::accepted(roots, &self.source, ContextClaim::Known(self.context))?;
        if self.sequence != current.sequence || !Arc::ptr_eq(&self.index, &current.index) {
            return Err(Error::StaleRequest);
        }
        Ok(())
    }
    fn same(&self, other: &Self) -> bool {
        self.sequence == other.sequence
            && self.source == other.source
            && self.context == other.context
            && Arc::ptr_eq(&self.index, &other.index)
    }
    fn active(&self, input: ActiveInput) -> Result<Active, Error> {
        let (mut active, utf16) = match input {
            ActiveInput::Source(a) => (a, false),
            ActiveInput::Utf16(a) => (a, true),
        };
        for range in [&mut active.selection, &mut active.composition]
            .into_iter()
            .flatten()
        {
            if range.start > range.end {
                return Err(super::Error::InvalidBoundary.into());
            }
            if utf16 {
                *range = self.index.boundaries.utf16_to_byte(range.start)?
                    ..self.index.boundaries.utf16_to_byte(range.end)?;
            }
            self.index.boundaries.range(range.clone())?;
        }
        Ok(active)
    }
}

/// An immutable root/reveal/identity-bound view. No old editor hitmap is stored.
pub(crate) struct Publication {
    identity: PublicationId,
    root: Root,
    active: Active,
    retained: Arc<RetainedPresentation>,
    reveal: RevealSnapshot,
}
impl Publication {
    pub fn identity(&self) -> PublicationId {
        self.identity.clone()
    }
    pub fn active(&self) -> &Active {
        &self.active
    }
    pub(crate) fn projection(
        &self,
        view: &View<'_>,
    ) -> Result<&crate::source_projection::Projection, Error> {
        self.check(view)?;
        Ok(self.reveal.projection())
    }
    pub(crate) fn styles(
        &self,
        view: &View<'_>,
    ) -> Result<&[crate::source_classifier::StyleSpan], Error> {
        self.check(view)?;
        Ok(self.retained.styles())
    }
    pub(crate) fn decorations(
        &self,
        view: &View<'_>,
    ) -> Result<&[crate::source_classifier::decorations::Marker], Error> {
        self.check(view)?;
        Ok(self.retained.decorations())
    }
    pub(crate) fn reasons(
        &self,
        view: &View<'_>,
    ) -> Result<&[crate::source_classifier::SourceReason], Error> {
        self.check(view)?;
        Ok(self
            .root
            .index
            .classification(view.source, ContextClaim::Known(self.root.context))?
            .reasons())
    }
    fn check(&self, view: &View<'_>) -> Result<(), Error> {
        let context = match &view.context {
            ContextClaim::Known(c) => *c,
            ContextClaim::Unknown => return Err(super::Error::UnknownContext.into()),
        };
        self.root
            .index
            .check(view.source, ContextClaim::Known(context))?;
        if self.identity != *view.identity {
            return Err(Error::PublicationMismatch);
        }
        if self.active != *view.active {
            return Err(Error::ActiveMismatch);
        }
        Ok(())
    }
    pub fn display(&self, view: &View<'_>) -> Result<&str, Error> {
        self.check(view)?;
        Ok(self.reveal.projection().display())
    }
    pub fn source_to_display(&self, view: &View<'_>, offset: usize) -> Result<usize, Error> {
        self.check(view)?;
        self.reveal
            .projection()
            .source_to_display(view.source, offset)
            .map_err(Error::Mapping)
    }
    pub fn display_to_source(
        &self,
        view: &View<'_>,
        offset: usize,
        bias: Bias,
    ) -> Result<usize, Error> {
        self.check(view)?;
        self.reveal
            .projection()
            .display_to_source(view.source, offset, bias)
            .map_err(Error::Mapping)
    }
    pub fn scope_is_raw(&self, view: &View<'_>, scope: Range<usize>) -> Result<bool, Error> {
        self.check(view)?;
        self.root.index.boundaries.range(scope.clone())?;
        Ok(self.reveal.is_raw(view.source, &scope))
    }
}

#[derive(Clone)]
struct Key {
    owner: Arc<Identity>,
    sequence: u64,
}
impl Key {
    fn same(&self, other: &Self) -> bool {
        self.sequence == other.sequence && Arc::ptr_eq(&self.owner, &other.owner)
    }
}
#[derive(Clone)]
struct Intent {
    key: Key,
    root: Root,
    active: Active,
}
impl Intent {
    fn same(&self, other: &Self) -> bool {
        self.key.same(&other.key) && self.root.same(&other.root) && self.active == other.active
    }
}

enum Work {
    Reuse(Arc<Publication>),
    Prepare(Option<Arc<RetainedPresentation>>),
}
pub(crate) struct Request {
    intent: Intent,
    work: Work,
}
pub(crate) struct Completed {
    intent: Intent,
    publication: Arc<Publication>,
}
impl Request {
    /// Derive only from the accepted SAME-root Classification. No parser or fuel.
    pub fn compute(self) -> Result<Completed, Error> {
        let publication = match self.work {
            Work::Reuse(publication) => publication,
            Work::Prepare(base) => {
                let classified = self.intent.root.index.classification(
                    &self.intent.root.source,
                    ContextClaim::Known(self.intent.root.context),
                )?;
                let retained =
                    base.unwrap_or_else(|| Arc::new(RetainedPresentation::new(classified)));
                let reveal = retained
                    .prepare_reveal(&self.intent.active)
                    .map_err(|e| Error::Projection(e.reason))?;
                Arc::new(Publication {
                    identity: PublicationId(Arc::new(Identity)),
                    root: self.intent.root.clone(),
                    active: self.intent.active.clone(),
                    retained,
                    reveal,
                })
            }
        };
        Ok(Completed {
            intent: self.intent,
            publication,
        })
    }
}

/// Separate completion/adoption from work, scoped to one owner and accepted root.
pub(crate) struct Session {
    owner: Arc<Identity>,
    sequence: u64,
    requested: Option<Intent>,
    accepted: Option<Completed>,
}
impl Default for Session {
    fn default() -> Self {
        Self {
            owner: Arc::new(Identity),
            sequence: 0,
            requested: None,
            accepted: None,
        }
    }
}
impl Session {
    pub(crate) fn invalidate(&mut self) {
        self.requested = None;
    }
    pub fn begin(
        &mut self,
        roots: &Roots,
        source: &Snapshot,
        context: ContextClaim,
        input: ActiveInput,
    ) -> Result<Request, Error> {
        // Every attempted input supersedes old pending work, including refusals.
        self.requested = None;
        let sequence = self.sequence.checked_add(1).ok_or(Error::SequenceLimit)?;
        self.sequence = sequence;
        let root = Root::accepted(roots, source, context)?;
        let active = root.active(input)?;
        let intent = Intent {
            key: Key {
                owner: self.owner.clone(),
                sequence,
            },
            root,
            active,
        };
        let work = match &self.accepted {
            Some(old) if old.publication.root.same(&intent.root) => {
                if old.publication.active == intent.active {
                    Work::Reuse(old.publication.clone())
                } else {
                    Work::Prepare(Some(old.publication.retained.clone()))
                }
            }
            _ => Work::Prepare(None),
        };
        self.requested = Some(intent.clone());
        Ok(Request { intent, work })
    }
    fn check(&self, roots: &Roots, intent: &Intent) -> Result<(), Error> {
        let current = self.requested.as_ref().ok_or(Error::StaleRequest)?;
        if !current.same(intent) {
            return Err(Error::StaleRequest);
        }
        intent.root.current(roots)
    }
    pub fn reject_partial(&self, roots: &Roots, request: &Request) -> Result<(), Error> {
        self.check(roots, &request.intent)?;
        Err(Error::Incomplete)
    }
    pub fn adopt(
        &mut self,
        roots: &Roots,
        completed: Completed,
    ) -> Result<Arc<Publication>, Error> {
        self.check(roots, &completed.intent)?;
        if self
            .accepted
            .as_ref()
            .is_some_and(|old| old.intent.key.same(&completed.intent.key))
        {
            return Err(Error::StaleRequest);
        }
        let publication = &completed.publication;
        if !publication.root.same(&completed.intent.root)
            || publication.active != completed.intent.active
        {
            return Err(Error::StaleRequest);
        }
        let result = publication.clone();
        self.accepted = Some(completed);
        Ok(result)
    }
    pub fn current(&self, roots: &Roots) -> Option<&Arc<Publication>> {
        let accepted = self.accepted.as_ref()?;
        self.check(roots, &accepted.intent).ok()?;
        Some(&accepted.publication)
    }
}

#[cfg(test)]
mod tests;
