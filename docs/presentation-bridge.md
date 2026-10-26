# Same-root public presentation bridge (Refs #1640)

The restricted `source_classifier::presentation` facade owns private root and
publication sessions. Call `begin_root` / request `compute` / `adopt_root`, then
`begin` / request `compute` / `adopt` explicitly outside native `compose`.
Only the closed canonical Live Preview configuration is admitted. Unknown options
or stateful callbacks retain their original observation-only path; they cannot
mint this authority. Root/reference observations and fuel remain from one parse.

A publication's checked view requires exact document/generation/bytes, closed
context, opaque PublicationId and full directed native active input: anchor/head,
composition, replacement and wrap affinity. Byte or UTF16 inputs are validated
independently against the accepted root's existing boundary index before taking
any replacement/composition union. An invalid input cancels prior pending
publication work. Root ownership and request ordering reject foreign/stale work.
Held immutable publications remain usable only with their own snapshot and input;
they do not authorize a newer editor or navigation result.

Checked views expose display/maps, styles, decorations, reasons and raw policy
from that same immutable publication. Raw parser, Classification,
RetainedPresentation and structural session/ticket handles remain private.
The shell's explicitly constructed `PreparedProvider` converts that view using
the existing style, heading, marker and rendered-block helpers. Construction is
outside `compose`: compose only compares exact source and the entire native active
state, then returns the already prepared immutable SourceProjection trait object.
It does not parse, lookup, reindex, derive reveal or create root authority.

This adapter is **unselected**. Production CachedProvider remains the selected display backend; Reader scheduling
is unchanged. Brain navigation request ownership is described below. The legacy document-links
inventory has different parsing semantics from classifier links and is not
replaced. Retained remap/raw-tail continuity is not supplied by a current-root
publication; replacing that production behavior with raw frames is not an
activation workaround. Those are separate activation obligations.

PublicationId is a core view identity, not a native presentation epoch.
ProjectionProvider::compose receives no epoch; existing native ProjectionSession
and PinnedProjection own epoch/layout/gesture binding. Full native active remains
part of the adapter lease even if core normalization permits the same underlying
view. Core completion, native installation, pinned prepaint and painted/post-frame
proof are distinct. No observer, frame proof or acceptance endpoint is added.

The ordinary 64KiB wall, 256KiB native guard, range/work/per-block caps,
full-parent cache key and canonical recompute remain. There is no fragment replay,
cross-edit reuse/performance claim, large-note admission, candidate/native build
or Reader activation. Genuine 130KB product acceptance remains a later gate.

## Separately bound navigation inventory

The private navigation inventory preserves the full immutable
`document_links::ParsedLink`, including label/title and `exact_range=false`.
It also prepares exactly the existing `CachedProvider` NoteLink conversion for
compatibility. Legacy ranges do not certify edit or marker boundaries. The parser,
normal factory/conversion and navigation actions retain their behavior; no resolver,
filesystem/network request or note-open action is introduced.

App lifetime/entity identity, complete workspace value, path, document/generation
and exact source bytes form the input binding. A separate opaque request-session
owner and checked attempt sequence govern async delivery; neither token claims
options equivalence. A new consumer lifetime is distinct even when its entity id
and visible input match the old one. No display name or hash stands in for equality.

`begin` supersedes before checking input availability/ownership/size. Explicit
`compute` calls the existing authored-link producer as cold work, outside compose.
Only it can construct a complete result; no partial completion flag/constructor is
exported. Adoption checks the actual live binding and latest pending ticket.
Failed, stale, foreign or duplicate adoption withdraws admission and requires a
fresh request. Live-query mismatch/unavailability likewise cancels pending/current
admission, so switching away and back cannot resurrect old rights. Held immutable
values can be read for their exact original input, not installed as current.

Repeated compute is repeated cold parsing, not a cache hit. Checked reads and
failed adoption do no parsing or derivation. Tests observe actual work through
the existing `link_parse_bytes` and `parse` diagnostics, with nonempty cold controls.

## Brain consumer request ownership

Brain's existing source-projection worker now begins the navigation session before
unavailable, refused or physically busy inputs. The latest queued request is
separate from the physical running flight. A foreground guard, constructed before
first poll, settles only its captured flight on completion or cancellation. The
stored foreground task is canceled when that owner retires. Only immutable
request/source data enters the background worker.

The worker computes the authored navigation inventory once, checks its immutable
view, then feeds the exact legacy links into shared CachedProvider construction.
The normal `classify` entry still uses the same producer and conversion. This is
one navigation parse per worker, not one total parser call: canonical display
classification remains separate. No parser or presentation authority is inferred
from legacy navigation ranges.

Completion checks the opaque latest request key and actual live input before
calling destructive adoption. Obsolete results cannot revoke a newer queued
request. Current admission and legacy provider installation occur in one UI
update. `current_source_links` uses a short fail-closed session borrow and actual
live owner/entity/workspace/path/document/generation/bytes, without parsing or
scheduling. Goal/source clear explicitly supersedes pending navigation. Queue
settlement is not proof of complete projection or painted presentation.

Valid edits keep the legacy provider installed for its existing retained remap and
raw-tail behavior. The deterministic BrainView regressions cover real loads,
refusals, latest queued input, live binding changes, goal clear, close/reopen and
two unpainted mutations, including intermediate legacy display/maps/styles/
headings/markers/blocks/raw policy and actual producer work controls.

Reader scheduling/park, full-active preparation, prepared publication selection,
continuity activation and native epoch/held-pin proof remain separate obligations.
Authored navigation inventory is separate from canonical presentation root/fuel
proof, action permission, publication identity and painted-frame proof. No
PresentationProof, observer, large-note admission or performance claim is added.
