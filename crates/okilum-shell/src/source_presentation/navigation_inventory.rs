//! Authored-navigation inventory, not presentation/root/frame authority.
//! The consumer must supply its actual live binding. No resolution or open action.
use gpui::EntityId;
use gpui_component::input::projection::SourceSnapshot;
use okilum_core::{document_links, source_classifier};
use serde_json::Value;
use std::sync::Arc;

struct Lifetime;
/// Actual app-consumer lifetime plus entity. Reopening even the same entity
/// creates a new value; clones refer to the original lifetime, not a display name.
#[derive(Clone)]
pub(crate) struct Owner {
    lifetime: Arc<Lifetime>,
    entity: EntityId,
}
impl Owner {
    pub fn new(entity: EntityId) -> Self {
        Self {
            lifetime: Arc::new(Lifetime),
            entity,
        }
    }
}
impl PartialEq for Owner {
    fn eq(&self, other: &Self) -> bool {
        self.entity == other.entity && Arc::ptr_eq(&self.lifetime, &other.lifetime)
    }
}
impl Eq for Owner {}

/// Complete app binding, not a display name or digest of workspace identity.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Binding {
    pub owner: Owner,
    pub workspace: Value,
    pub path: String,
}
#[derive(Clone)]
pub(crate) struct Input {
    pub binding: Binding,
    pub source: SourceSnapshot,
}
impl Input {
    fn same(&self, other: &Self) -> bool {
        self.binding == other.binding
            && self.source.stamp == other.source.stamp
            && (Arc::ptr_eq(&self.source.text, &other.source.text)
                || self.source.text == other.source.text)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Error {
    Unavailable,
    SourceLimit,
    Stale,
    SequenceLimit,
}

// This token proves request ownership only, never parser/options equivalence.
#[derive(Default)]
struct RequestOwner;
#[derive(Clone)]
struct Ticket {
    owner: Arc<RequestOwner>,
    sequence: u64,
    input: Input,
}
impl Ticket {
    fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.owner, &other.owner)
            && self.sequence == other.sequence
            && self.input.same(&other.input)
    }
}

#[derive(Clone)]
pub(crate) struct Request(Ticket);
#[derive(Clone)]
pub(crate) struct Completion(Arc<Inventory>);

/// Logical attempt identity; physical worker ownership is managed by the UI.
#[derive(Clone)]
pub(crate) struct RequestKey {
    owner: Arc<RequestOwner>,
    sequence: u64,
}
impl RequestKey {
    fn matches(&self, ticket: &Ticket) -> bool {
        Arc::ptr_eq(&self.owner, &ticket.owner) && self.sequence == ticket.sequence
    }
}
impl Completion {
    pub fn view(&self, input: &Input) -> Result<View<'_>, Error> {
        self.0.view(input)
    }
}
impl Request {
    pub fn key(&self) -> RequestKey {
        RequestKey {
            owner: self.0.owner.clone(),
            sequence: self.0.sequence,
        }
    }
    /// Explicit cold work on the existing background executor, never in compose,
    /// a query, or failed adoption. A retry honestly performs another cold parse.
    pub fn compute(self) -> Completion {
        let parsed = document_links::parse(&self.0.input.source.text);
        // EXACT existing CachedProvider conversion, not Classification::links_for.
        // Legacy ranges do NOT certify edit/marker boundaries; keep ParsedLink's
        // exact_range flag alongside them, including false outcomes.
        let legacy = parsed
            .iter()
            .map(|link| source_classifier::NoteLink {
                label: link.range.clone(),
                range: link.range.clone(),
                target: link.target.clone(),
                wiki: link.wiki,
            })
            .collect();
        Completion(Arc::new(Inventory {
            ticket: self.0,
            parsed,
            legacy,
        }))
    }
}

/// Only compute constructs this complete immutable value. There is no public
/// partial constructor/flag and no accessor that claims it is current elsewhere.
pub(crate) struct Inventory {
    ticket: Ticket,
    parsed: Vec<document_links::ParsedLink>,
    legacy: Vec<source_classifier::NoteLink>,
}
impl Inventory {
    /// Held values may be read only with their own exact binding/snapshot. They
    /// cannot install themselves or establish current session/navigation rights.
    pub fn view(&self, input: &Input) -> Result<View<'_>, Error> {
        if !self.ticket.input.same(input) {
            return Err(Error::Stale);
        }
        Ok(View {
            parsed: &self.parsed,
            legacy: &self.legacy,
        })
    }
}
pub(crate) struct View<'a> {
    parsed: &'a [document_links::ParsedLink],
    legacy: &'a [source_classifier::NoteLink],
}
impl View<'_> {
    pub fn parsed_links(&self) -> &[document_links::ParsedLink] {
        self.parsed
    }
    /// Compatibility data only. In particular, a legacy NoteLink range is not
    /// a substitute for the corresponding ParsedLink.exact_range decision.
    pub fn legacy_links(&self) -> &[source_classifier::NoteLink] {
        self.legacy
    }
}

enum State {
    Pending(Ticket),
    Current(Arc<Inventory>),
}
impl State {
    fn ticket(&self) -> &Ticket {
        match self {
            Self::Pending(ticket) => ticket,
            Self::Current(inventory) => &inventory.ticket,
        }
    }
}

/// One real consumer lifetime. Drop/recreate on close/reopen; opaque owner
/// identity rejects old deliveries even if every visible input and sequence match.
pub(crate) struct Session {
    owner: Owner,
    request_owner: Arc<RequestOwner>,
    sequence: u64,
    state: Option<State>,
}
impl Session {
    pub fn new(owner: Owner) -> Self {
        Self {
            owner,
            request_owner: Arc::new(RequestOwner),
            sequence: 0,
            state: None,
        }
    }
    /// None matches the consumer's unavailable snapshot/binding path. Capture and
    /// supersede the attempt BEFORE preflight, including refusals and overflow.
    pub fn begin(&mut self, input: Option<Input>) -> Result<Request, Error> {
        self.state = None;
        self.sequence = self.sequence.checked_add(1).ok_or(Error::SequenceLimit)?;
        let input = input.ok_or(Error::Unavailable)?;
        if input.binding.owner != self.owner {
            return Err(Error::Unavailable);
        }
        if input.source.text.len() > source_classifier::MAX_BYTES {
            return Err(Error::SourceLimit);
        }
        let ticket = Ticket {
            owner: self.request_owner.clone(),
            sequence: self.sequence,
            input,
        };
        self.state = Some(State::Pending(ticket.clone()));
        Ok(Request(ticket))
    }

    /// Non-destructive outer worker check. An obsolete result must not enter
    /// adopt, whose failure deliberately withdraws current admission.
    pub fn is_pending(&self, key: &RequestKey, live: Option<&Input>) -> bool {
        let Some(State::Pending(ticket)) = &self.state else {
            return false;
        };
        Arc::ptr_eq(&ticket.owner, &self.request_owner)
            && ticket.sequence == self.sequence
            && key.matches(ticket)
            && live.is_some_and(|live| ticket.input.same(live))
    }

    /// A failed/duplicate/foreign delivery withdraws admission, never revives an
    /// older current inventory. Start a new request to recover; no parse occurs.
    pub fn adopt(
        &mut self,
        completion: Completion,
        live: Option<&Input>,
    ) -> Result<Arc<Inventory>, Error> {
        let previous = self.state.take();
        let Some(State::Pending(ticket)) = previous else {
            return Err(Error::Stale);
        };
        if !Arc::ptr_eq(&ticket.owner, &self.request_owner)
            || ticket.sequence != self.sequence
            || !ticket.same(&completion.0.ticket)
            || !live.is_some_and(|live| ticket.input.same(live))
        {
            return Err(Error::Stale);
        }
        self.state = Some(State::Current(completion.0.clone()));
        Ok(completion.0)
    }

    /// Reads compare actual live owner/entity/workspace/path/stamp/bytes. A live
    /// mismatch/unavailable binding cancels pending as well as current admission,
    /// so switching away and back cannot resurrect an old completion.
    pub fn current(&mut self, live: Option<&Input>) -> Option<Arc<Inventory>> {
        let matches = self.state.as_ref().is_some_and(|state| {
            let ticket = state.ticket();
            Arc::ptr_eq(&ticket.owner, &self.request_owner)
                && ticket.sequence == self.sequence
                && live.is_some_and(|live| ticket.input.same(live))
        });
        if !matches {
            self.state = None;
            return None;
        }
        match &self.state {
            Some(State::Current(inventory)) => Some(inventory.clone()),
            _ => None,
        }
    }
}

#[cfg(test)]
#[path = "navigation_inventory/tests.rs"]
mod tests;
