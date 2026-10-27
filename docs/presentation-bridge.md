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

This adapter is **unselected**. Production CachedProvider remains the selected display backend. Brain navigation
request ownership and Reader legacy delivery are described below. The legacy document-links
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

Full-active preparation, prepared publication selection, continuity activation and
native epoch/held-pin proof remain separate obligations. Brain lifecycle proof
alone is not Reader park proof; Reader-specific delivery is described below.
Authored navigation inventory is separate from canonical presentation root/fuel
proof, action permission, publication identity and painted-frame proof. No
PresentationProof, observer, large-note admission or performance claim is added.


## Reader legacy classification delivery

Reader's existing LivePreview worker uses the same opaque Session/shared producer
to deliver CachedProvider. It computes one strict authored-link inventory per
worker, plus the separate canonical display parse. The actual Reader SourceClick
still calls parse_in_vault and preserves vault-dependent fallback, resolution,
save-before-action and ambiguity handling. No strict inventory is promoted to
Reader navigation or canonical-root authority.

A new Editing owns one Reader/input lifetime, pinned Reader root/path/selected
path, exact FileEditor path and document kind. Paths are compared as PathBuf and
encoded losslessly for session identity, without new FS probes. Completion checks
actual live Reader/input/context/source and latest opaque request before destructive
adoption. It installs the legacy provider with the current palette in one UI update.
Latest queued work is separate from an owned physical flight; its foreground guard
exists before first poll, and cancellation settles only that flight.

Editing::park cancels pending/queued delivery before disabling input and moving
FileEditor to the worker. The existing input, lifetime and immutable legacy display
stay in the foreground reassembly closure. Move completion explicitly resumes only
after live path/store/text synchronization, including equal-value set_value paths.
Retired Editing/close/new source gets a new owner. A held provider may reuse its exact
current snapshot after full binding checks; this does not fake Session adoption or
new current navigation rights. Interrupted/source-changed work is honestly cold.

Valid pending edits retain the selected legacy remap/raw-tail presentation. Unchanged
save echoes and theme updates reuse classification; external changed bytes are new
work. No save/recovery/autosave/IME/action ordering or live block renderer is changed.
Native full-active preparation, PreparedProvider selection, native epoch/held-pin
proof, observer/PresentationProof and130KB admission remain separate follow-ons.
The existing caps, full-parent key and canonical recompute/fuel remain unchanged;
no cross-edit performance or Stable acceptance is claimed.


Reader regression receipts distinguish full asynchronous same-byte save echoes
(`apply_vault_changes` through executor drain, with zero classification/reinstall
and stable native state) from deferral at a delivered consumer boundary. The latter
starts a real autosave and delivers the existing reconciliation callback with actual
fixture disk input while that save is pending, then drains actual save/refresh work.
It does not prove that the whole entry/inventory pipeline delivers before autosave;
that ordering remains unproven and is not inferred from a favorable schedule.
