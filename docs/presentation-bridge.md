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

This adapter is **unselected**. Production CachedProvider classification, Reader
and Brain backend selection/schedulers are unchanged. The legacy document-links
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
