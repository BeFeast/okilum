# Point-and-click Log filters

Issue #2389 adds generic cell actions, column value pickers and an editable chip
bar. The same controls apply to indexed files and externally replaced Log views.
The optional advanced query field displays the entire applied query, including
all enabled chips. A valid manual edit is split back into recognized chips and
advanced conditions. Invalid text retains the last applied query and reports its
parse error. Clear all also clears disabled chips, free text, level and time.

## Contracts for remote sources

Public types live in okilum_core::log::chips:

- FilterChip carries a FieldRef, typed FilterValue list, FilterMode, enabled and
  FilterExecution. Execution defaults to Local; Server is reserved for source
  adapters to mark where a filter is executed. This change evaluates every chip
  through the existing local query engine.
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
  A future server provider can return its own scope label and error.
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
introduced.

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
views. Run the entire okilum-shell crate as well as core log tests on a build slot.
