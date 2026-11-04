# Manager and Board rendering (#2464 / #2465)

Board v2 filtering owns the shared source projection and ordered membership for
List and Kanban. Both presentations retain an `Arc<Binding>` from that projection;
Kanban viewport cards must not copy the complete filtered binding per frame.
Source read completions invalidate the filter cache explicitly. Filter model
changes replace it once, and freshness/merge transitions use the exact expiry
instants already defined by Board v2. Scroll, resize, selection and unrelated
Manager notifications reuse it. Do not restore the older parallel Kanban cache
or invalidate the shared projection on every Board notification.

List retains its fixed-height virtual stream and prepared row index. Kanban
retains natural card heights, viewport layout, offscreen geometry hints and
keyboard/focus-aware disclosures. Main's ordering comparator and stable binding
order for two missing source issues apply in both presentations.

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

Issue cards use vertical block flow at the lane's resolved width. Existing
8px inter-child gaps become leading margins, and the PR link keeps its intrinsic
width in a horizontal row. Titles, status content, source links, disclosures and
natural reflow retain the same geometry; the extra outer flex intrinsic-size
negotiation is unnecessary because the lane already measures at a definite width.

The macOS native-Manager compilation introduced by #2524 retains its
`native_manager` modules, fields, fixtures and platform shortcuts. Linux-only
chrome/Reader caches and native profiling remain guarded on Linux; the portable
composer keeps its native guard, while macOS uses the same uncached chrome and
WKWebView runtime selection as main. Harness traces inside portable modules are
explicitly Linux-only so enabling the harness cannot reference a missing module.

Lane height hints retain an ordered index keyed by the same immutable shared
filter projection as both Board presentations. Source/filter changes prepare
and prune membership once, retaining existing height/focus hints by identity.
Scroll and splitter frames sum those mutable hints directly; they do not rebuild
and probe a hash set/map for every canonical card. Both the filtered Kanban test
and real 240-card splitter regression check zero membership preparations during
resize, with nonempty initial/source/filter preparation controls.

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
