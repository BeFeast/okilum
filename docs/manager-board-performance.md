# Manager and Board rendering (#2464 / #2465)

Board v2 filtering owns the shared source projection and ordered membership for
List and Kanban. Both presentations retain an `Arc<Binding>` from that projection;
Kanban viewport cards must not copy the complete filtered binding per frame.
Source read completions invalidate the filter cache explicitly. Filter model
changes replace it once, and freshness/merge transitions use the exact expiry
instants already defined by Board v2. Scroll, resize, selection and unrelated
Manager notifications reuse it. Do not restore the older parallel Kanban cache
or invalidate the shared projection on every Board notification.

List retains its fixed-height virtual stream and prepared row index. Main #2555
also uses that same prepared projection for fixed-width, independently virtual
Kanban lanes, compact three-row cards, the eight-row Queue preview with its List
handoff, and the Verified rail. All source ordering, counts, collapse preferences,
scroll handles and separate details remain. The older natural-height virtual
lane is superseded by this production architecture, rather than mounted beside
it. `Arc<Binding>` and prepared groups survive viewport changes in both views.

Reader geometry has a separate signal from source publications. The Linux
splitter owns pointer capture instead of triggering a whole-window DnD refresh.
Manager source display still publishes through the cached Board boundary;
composer edits and history scroll do not rebuild the sibling Board.

The synthetic Reader-mounted performance regression exercises native wheel,
splitter, disclosure and picker Escape. Filter/List regressions also check real
search events, source invalidation, shared binding identity and bounded row work
through resize. Native timing follows `scripts/perf/manager-board/README.md`.

The shared search field also exposed gpui-kit's redundant input notifications:
every unchanged paint republished layout and scroll state, invalidating cached
Board ancestors on unrelated Manager scroll frames. Patch 0124 backports the
notification guards from upstream `longbridge/gpui-kit` commit
`16a3051c811a987e8f78390ebf57104449c0d2e5` (PR #2988) onto the pinned tree.
Unchanged paint/scroll stays silent; changed geometry, editing and selection
still publish. The upstream regressions cover repeated cached paint, changed scroll and
clamped offsets, diagnostic dismissal, and editing/selection/bounds controls. gpui core remains unmodified.

Filter chips and mode options use the existing compact Base Ghost button path.
Their labels/counts sit directly in the control, removing the styled Button's
additional full-height flex wrapper during every splitter layout. Small-control
size, Ghost/selected colors, focus rings, keyboard activation, accessibility
labels and all filter/view callbacks are retained in both presentations.

Compact issue/local disclosures and Queue rows use Base's complete button
interaction around main's unchanged content geometry. Keyboard activation keeps
the disclosure focus alive for Space/Enter; pointer activation retains main's
Board focus. Linux compact agent rendering reads retained ManagerDisplay rather
than the whole Manager entity, matching List's cache boundary. Live source PR
links remain available when a manifest has no explicit PR annotation.

The macOS native-Manager compilation introduced by #2524 retains its
`native_manager` modules, fields, fixtures and platform shortcuts. Linux-only
chrome/Reader caches and native profiling remain guarded on Linux; the portable
composer keeps its native guard, while macOS uses the same uncached chrome and
WKWebView runtime selection as main. Harness traces inside portable modules are
explicitly Linux-only so enabling the harness cannot reference a missing module.

Canonical row membership is prepared once when the shared projection or collapse
state changes. Both List rows and Kanban groups reuse it on resize/scroll.
Regressions require nonempty initial/source/filter preparations and none during
splitter geometry changes. Independent build and actual request-layout probes
require nonzero viewport work bounded to 64 rows per frame. Real physical wheel
input must move the Working lane and reveal a final source card with actions.
GPUI's default wheel handling remaps input onto a container's scroll axis. The
horizontal Kanban container and its vertical lanes/Queue explicitly restrict
input to the original axis, so a vertical wheel cannot also pan sibling lanes
away, and horizontal navigation cannot move a lane vertically. The mounted
regression exercises both axes, retains the real lane offset while collecting
cached debug selectors, and activates the final Working card after scrolling.

The backported caret-consumer regression subscribes explicitly to input
notifications across its cached Panel on pinned GPUI 0.3.3. It still requires a
nonempty initial painted caret, a real selection movement, and the render
consumer observing the updated geometry. This fixture adaptation does not alter
the upstream production notification guards or force the consumer to render.

The caret fixture seeds its cached consumer once after initial paint, because
pinned GPUI registers the first frame's dependencies after paint completes.
Subsequent selection changes must update the render consumer through ordinary
notifications; no refresh is forced for that assertion.

Chat message surfaces also use block flow at the scroller's resolved width.
The two existing 4px gaps become leading margins on Markdown and attachments;
user surfaces retain their 640px maximum and trailing alignment, while Manager
surfaces retain their 720px maximum and leading alignment. Header controls,
Markdown selection, code actions, attachment transfers and scroller ownership
are unchanged. This removes nested flex intrinsic-size negotiation for the
rich rows actually exercised during native splitter measurement.

The #2531 issue dock uses the same retained Linux ManagerDisplay for exact-run
progress and thread links. It resolves the selected issue and mapping diagnostic
through indices built with the shared projection, including when row filters hide
the selection. Opening, resizing and repainting the dock do not scan every issue
or subscribe its parent to the Manager composer. The mounted reference keeps its
wheel/splitter bounds and checks keyboard opening/Escape against the issue tab.

On Linux, Board notifications invalidate Reader's existing layout signal, so the
center and issue dock update without notifying observers of the whole Reader.
The fixed header remains cached through splitter geometry changes; actual Reader
source publications still invalidate it. The mounted regression installs the
same Board observer path as production and retains its zero-header-rebuild gate.

Issue creation time remains in the flattened waiting Report source metadata
introduced by #2533. The issue history reads that same timestamp; a second
serde field would consume it before the waiting projection could see it.
