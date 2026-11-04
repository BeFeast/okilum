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
