# Native Tasks presentation (#636)

Build on the merged registry, layout parser, preferences, canonical Reader
snapshot and indexed safe-write boundary. Markdown remains the source of truth.

## Presentation integration

- Select from the accepted canonical source and app mappings, never rendered
  Markdown or a fresh disk read. Cache parsing by source/preferences/local date;
  source publication, note navigation and local midnight invalidate the result.
- Mount the Tasks surface after file/timeline/source-editor routing in
  `Reader::render_main`. Keep the document header and its Source/preview action;
  Show source must work even when selection or layout parsing fails.
- Reuse the existing Tasks rows, note backlinks, due pills and carried-copy
  expansion. Preserve each query's source identity while applying parsed section
  order, density and grouping. Counts describe the visible result; empty sections
  have no zero-count sentence. Add native filter/grouping controls inline.
- Unknown views and invalid definitions show ordinary Markdown with a clear
  fallback reason. Do not partially render an invalid dashboard. Ordinary
  Markdown Tasks blocks retain their existing behavior.

## Mutation integration

Capture the immutable index and exact occurrence displayed by the row. Dispatch
checkbox, due-date and snooze actions through `apply_indexed` on a worker; never
replace the captured evidence with a newer index at click time. Collapsed carried
copies must be expanded before an occurrence can be changed. One overlay toast
offers the retained receipt's Undo. Conflicts refuse the mutation. Keep FileEditor
save and editor ownership unchanged. Snooze changes scheduled date, not due date.

## Verification

Native regressions cover frontmatter-only view switching, malformed/unknown-view
fallback, Source/preview round trip, section order and grouping, midnight query
refresh, stale displayed evidence, source navigation and collapsed occurrences.
Capture the same Linux fixture before/after in light and dark; check all ten UI
rules. Published-beta native QA remains with the dedicated muninn session.

## Native navigation and dates

Incoming heading links land on the unique matching native section in display
order. Missing or duplicate section labels show a notice with the Source escape
rather than scrolling a hidden Markdown surface. The Contents list retains
source identities even when layout reorders sections.

The reschedule popover combines quick due/scheduled actions with the shared
calendar for arbitrary due dates. Its subscription captures the displayed task,
index and vault when opened; selecting a date never refreshes that evidence.
Calendar month/year navigation leaves the popover open; choosing a day dismisses
it and uses the same worker/save/Undo path as quick actions.
