# Point-and-click Log filters

Issue #2389 adds generic cell actions, column value pickers and an editable chip
bar. The same controls apply to indexed files and externally replaced Log views.
The optional advanced query field displays the entire applied query, including
all enabled chips. A valid manual edit is split back into recognized chips and
advanced conditions. Invalid text retains the last applied query and reports its
parse error. Clear all also clears disabled chips, free text, level and time.

## Header and query row

Reader-hosted logs put Find in records, Level, Time range, Follow and menu actions
in the pinned document header. Standalone log hosts use the same descriptors in
their own compact header. Find narrows text without case sensitivity; Enter keeps
the filter and focuses rows, Esc clears and returns. FocusLogFilter remains the
alias for existing user bindings.

Query opens the single secondary row. A query, any chip (even disabled), regex
editing/error or an investigation keeps it present. That row owns the query
field/error, Recent queries, Regex, chips and the only Clear all. At narrow widths
chips open from one counter. Counter and source/scope notices are in the bottom
bar; the message-field chooser is in Columns. Copy raw line and whole-file/path
commands are under Document actions > Copy, with no separate Copy button.

## Contracts for remote sources

Public types live in okilum_core::log::chips:

- FilterChip carries a FieldRef, typed FilterValue list, FilterMode, enabled and
  FilterExecution. Execution defaults to Local. Generic views evaluate chips
  through the existing local engine. A remote adapter opts into server ownership
  with LogView::enable_server_filters; Server chips then leave the local matcher
  and FilterChipsChanged carries the complete typed state to the adapter. The
  advanced query box continues to display the combined chip expressions.
- FilterValue distinguishes Missing from String, Number, Bool, Null, Array and
  Object. Strings use decoded field text. Other values preserve their canonical
  source text. Empty strings and the words null/false remain strings.
- FilterChip::expr and combined_query compile chips to the existing query AST.
  split_query recovers chip groups while keeping arbitrary advanced expressions.
  Exclude negates the entire include group, including records without that field.
- ColumnCountProvider::count receives the requested FieldRef and immutable
  CountSource slices and returns Result<ColumnCounts, String>. ColumnCounts
  includes its scope label and ValueCount list. LoadedRowCountProvider labels its
  results "in loaded rows" and counts each record once per distinct typed value.
  The Kibana provider returns window counts, bounds and quality with its own
  scope label. Its picker can independently switch to loaded-row counts.
- ColumnCountProvider::canonical_field defaults to identity. Remote providers
  bind display roles to a unique underlying field before recording a chip. This
  preserves editability after a server query returns zero rows; ambiguous roles
  remain explicit. Kibana role identities are derived with EventWindow off the
  UI thread.
- LogView::set_count_provider installs an Arc of the provider. Calls execute on
  the background executor when a picker opens. Reopening takes a fresh snapshot;
  an already open picker shows the snapshot it counted. Counting and value search
  never run in draw; the picker virtualizes its visible values.

The existing parser recognizes typed(field, kind, "text") conditions. Field names
and value text use the existing query lexer and JSON escaping. Typed equality
coexists with the legacy coercing operators, whose behavior is unchanged. @time
and @source refer to the displayed role/source inside typed conditions; quoted
keys with those names remain raw fields. Source expressions are bound per file
before the ordinary engine evaluates it. No source-specific matching engine is
introduced. The guided Kibana adapter compiles supported scalar chips to server
DSL; unsupported semantics remain errors or explicitly local chips. See
[guided investigation](kibana-guided-investigation.md).

SavedQuery keeps filter_chips (including disabled state and execution location)
and optional columns alongside the existing canonical query. Old saved queries
retain their default behavior and do not replace the current column layout.
Same-source local reopen transfers the current view state; stream/remote snapshot
replacement and follow append retain it in the existing LogView entity.

## Validation

Core tests cover all primitive types, missing/null, escaping, query round trips,
multiselect, exclusion, disabling, clearing, appended records, source binding,
row-count deduplication and saved-state compatibility. GPUI tests deliver mouse
press and release in separate frames for cell context actions, header multiselect,
chip editing, disabling, removal, clearing, follow, same-source reload and saved
views. For the guided Kibana gate, run filtered reader_log and shortcut shell
targets (two test threads), plus core log/remote_log tests, in a build slot. An
unrelated full-suite abort before the target tests is not evidence for them.
