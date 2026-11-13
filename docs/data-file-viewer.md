# Read-only data files (#2669)

## CSV and JSON tables (#2735, decision #1532)

CSV/TSV and JSON arrays open as read-only tables. JSON offers Table, Tree, Formatted and Raw through one header switch; normal valid JSON objects
open in Formatted. JSONC and JSONL/log routing remain unchanged.
Objects in an array use the union of their top-level keys, nested values stay
compact JSON, missing fields stay empty and explicit null displays `null`.
Arrays of arrays use numbered columns; scalar arrays use a Value column.

Click a column heading for ascending, descending, then source order. Numeric
columns sort numerically; other columns use case-sensitive text order. Find in
loaded rows matches a substring across every cell, with optional Match case. Sorting and
filtering run on workers; obsolete results cannot replace newer queries. Copy
uses displayed row order. Find selects the first matching cell; stepping reveals
each hit on both axes, and clearing the query preserves source-cell selection.
Both rows and columns are virtualized. The no-header CSV option retains every
source row and provides numbered sortable column headings.

The first preview is bounded by 1 MiB, 1,000 displayed rows, 64,000 cells,
256 columns and 64 KiB per field. Load more rows explicitly re-reads up to
32 MiB, 100,000 rows and 500,000 cells with the same column/field bounds.
Counts identify the loaded scope and partial files. Filters and sorts cover
only that scope; the limit notice never claims the whole file was searched.
Cut records are omitted rather than displayed as complete values. Oversized,
malformed or unsupported JSON still has the Raw view.

Open in DuckDB starts an installed DuckDB CLI using its documented
`-cmd "CREATE VIEW ..." -ui` interface, exposing the full canonical file as
`okilum_data` in an independent in-memory database. CLI arguments bypass the
shell; SQL literals and file glob characters are escaped. The CLI process is
reaped independently of the Reader. Missing/failed CLI launches display a
toast with a Get DuckDB action. Installation and extension downloads belong to the
external CLI. Okilum adds no dependency, database engine, Parquet/.duckdb
viewer, SQL input or bundled asset; the dependency set and packaged assets are unchanged.

## Source viewing

JSON/JSONC, XML/XSD/XSL, TOML, YAML and TXT use the Reader's code viewer.
XML uses the pinned MIT tree-sitter-xml 0.7.0 grammar; no network resolution or
DTD/entity expansion occurs. Existing source-format editors stay explicit; XML
preview eligibility never grants save/recovery authority. Viewing, Pretty, Find,
folding and copying do not write canonical files.

The viewer has line numbers, Find with a match count, native selection Copy,
Copy node at the caret, Fold all/Unfold all and per-node gutter folding. Copy node
uses a parsed immutable snapshot of the displayed Raw/Pretty revision and copies
the smallest containing object/array/property, XML element, TOML property/table or
YAML property/item. A stale parse/search cannot replace a newer display revision.
Open with default app uses the existing guarded file action.

### YAML, TOML and XML (#2813)

YAML, TOML and XML get the semantic Tree at any size, with the JSON row rules:
a key or tag name and, on the right, a count or a short value. YAML lists keys
and sequence items (a multi-document file lists its documents); TOML lists keys
and tables in source order, and consecutive `[[name]]` blocks are one array row
whose items are the blocks; XML lists elements, with the `id`, else the `name`
attribute (namespaced `xml:id` counts), else the number of child elements, else
the short text on the right. Attributes, comments and text are not rows, and the
XML root element is always open. The view switch is Tree / Raw: Raw is the
default, Tree for a long single-line file, and an explicit choice is remembered
per format. Find in Tree works for a normal-size file. The rows are built from
the whole-file parse on a worker with an explicit stack, never recursively.

Only YAML has a validity verdict: `serde_yaml` (multi-document files and anchors
pass). A rejected file shows the badge by the name, a disabled Tree with the
reason and the bottom line «Not valid YAML.» with the position and Go to error.
TOML and XML have no strict parser in the app, so they never get «not valid»:
the tolerant parse feeds Tree, and a grammar error shows the badge and the
bottom line «Syntax problem. Line L, column C.» with Go to error. A corpus of
real valid files (Compose, GitHub workflows with anchors, multi-document
Kubernetes manifests, Cargo and pyproject manifests, Atom, SVG, POM, XSD) must
show neither.

Files up to 64 MiB load on a worker with a bounded read even when the file grows.
Parsing, JSON formatting and full-file search run on workers. All CodePreview formats
above the 4 MiB UI editing cap stay read-only, including JSON and TXT. This is UI admission:
canonical FileEditor checks and the independent 64 MiB source-tool budget remain unchanged. Large multi-line files start without soft
wrap; the existing wrap preference continues for smaller files.

A line longer than 64 KiB uses 32 KiB UTF-8-safe pages, with a bottom percentage, Previous/Next part, a toolkit scrubber and whole-file Find with total count and navigation.
Slider events coalesce to the last requested part before the next frame.
Copy selection is exact within the displayed page; Copy node can cross any number
of pages. Search can match across a page boundary. A spanning match selects its
part on the current page. Gutter numbers in the explicitly paged Source view are local to the page.
Structured long-line files below the 8 MiB automatic threshold initially show a virtualized Tree over the
complete AST, with individual node folding, Fold all/Unfold all, exact
whole-node Copy and Show in raw text navigation. Valid JSON has a semantic Tree
with named properties and indexed array items, including scalar items; its
synthetic root is hidden and Tree is absent for a childless root. Rows show a key
and child count or short value, without a source-line column or type words;
only expanded rows are visible and only the viewport is rendered. The index is
built iteratively on a worker from the complete immutable parse, within the
existing 64 MiB file preview limit. Pretty's formatting bounds remain separate.
Tree folding is independent of page boundaries, and value previews read at most
96 characters. Tree never displays byte-page controls. Whole-file Find keeps
Tree open, unfolds the matching node's ancestors, scrolls to and highlights the
hit, and provides bounded context when a match is beyond the default value
preview. Find in paged Source still navigates to the matching Source page; its
keyboard shortcut targets whole-file Find rather than a page-local search.
No placeholder text is inserted into
the source or copied as file contents. JSON Pretty can also restore the complete
multi-line code view without changing its file.

Tree rows stay at 32 px with a clipped, single-line ellipsis label, including
narrow windows. The Contents panel accepts only a complete display snapshot; a
32 KiB source page never supplies a partial document outline. Large complete
inputs retain the outline's existing 256 KiB budget and keep Linked from visible.
Leading underscores on ordinary non-Markdown data files are not a hidden marker.
Underscore Markdown notes/folders and dot paths retain the vault's existing
hidden-file preference.

## Variant A CodePreview header (#2809)

One `View` value owns Table, Tree, Formatted or Raw. Availability distinguishes
available, disabled with a reason, and absent. The first bounded Raw text is
shown before parsing/indexing begins. The raw document and formatting limits are
then prepared on a worker; a formatted document is built only when requested
(including the default Formatted view). The default presentation is applied only
while that provisional text remains untouched. Scroll, selection, caret, Find
or an explicit view command pins Raw for this open, including interaction while
the default formatter is still running; this never changes per-type memory.
If an untouched default unmounts a programmatically focused text Input, focus
moves to the mounted CodePreview target. Raw/Formatted snapshots are retained;
switching, folding and paging never reparse or reset
Tree folds. Fold all never changes the selected view. Only explicit user choices
in unguarded files update the per-type map; Go to error, Show in raw text node navigation
and background completion never write memory. The map covers json, json-array,
yaml, toml, xml, csv and tsv. CSV and TSV default to Table with lazy Raw loading;
JSON-array memory is independent. Legacy `json_raw` migrates to
the JSON-object entry and is omitted from subsequent writes.

Size guards precede memory: over 8 MiB opens Raw without formatting; a line over
64 KiB opens Tree when available, otherwise Raw. Then a supported remembered view
wins, followed by Table for arrays, Formatted for valid JSON, Raw otherwise.
Formatting refuses only nesting over 128 or the existing 64 MiB formatted-output
budget. Invalid JSON disables Tree and Formatted without writing any source.
Its owned serde error survives above the automatic threshold; Go to error maps
its byte offset to the correct Raw part. Read-only editor diagnostics use the
same whole-file error, translated into the visible page.

The shared 48 px header has a reusable 28 px segmented switch. At narrow width
it keeps icons and the active label before Fold/Unfold yield into Document
actions. A single-view source file has no switch. All tooltips use live bindings.
Bare/native hosts and ZIP entries use the same builders with one view menu;
ZIP merges them with its breadcrumb in one measured compact bar. Copy holds
Copy selection (disabled in Tree) and Copy node with `Copied node · N lines`.
The old tools, Structure/Source, page and Pretty/Raw rows are removed. The shared
header field owns read-mode Find in every format; there is no whole-file row or
toolkit strip. The table's inner rows and all edit chrome remain for their ordered
steps. While JSON Table is active the field is absent and Ctrl/Cmd+F focuses its
existing filter.

The name has a read-only lock with its reason and, independently, a warning that
jumps to a JSON error. Storage writability is never inferred from opening a file;
after the existing file-identity/storage-policy admission, an explicit Edit
attempt probes an empty temporary sibling in the document
directory, immediately removes it and records only that location's permission or
read-only failure. Private recovery-state failures never mark the vault read-only.
One bottom line chooses invalid JSON, formatting, large/raw or long-line,
then size/storage read-only in that order; a view-only format such as XML uses
only the name lock. The lock always retains the read-only reason. View switches
never focus the read-only text Input; Tree retains a mounted CodePreview target.
Async formatting preserves focus and never applies after Edit entry or a newer
view request. An explicit Formatted request completes even if Find gains focus:
the same whole-file Find remains mounted and usable without moving focus.
Edit teardown, including Close, keeping draft after a conflict, resumes any
retained CodePreview; the recovery draft and incoming file stay separate. Part commands apply only to changed Raw/Formatted pages. Copy-node
enablement caches tree-sitter lookup by snapshot, page and caret. Error-tolerant
source grammars keep any fold ranges the editor can fold.
Raw and Formatted show position controls when paged. The right sidebar remains
unchanged until #2814; it will consume the existing document snapshot API.
Contents can reuse `CodePreview::document_snapshot()` without parsing again.
It returns the same immutable `Arc<reader_code_file::document::Document>` used
by Tree and Find, with full text, `json_tree` validity, preorder `structure` rows
(parent, depth, subtree end, original line and exact byte range), `row_label`,
`row_at` and `find`. Node indices/ranges belong to that snapshot; consumers must
replace them together when the selected view publishes a different snapshot.

Console output is best-effort: a desktop launcher's closed stdout/stderr pipe
must not panic the Reader. `reader-diagnostic.log` records native platform close
requests, actual window removals and their window ids/remaining counts, plus
platform warning/error messages. A removal without a recorded close request is
explicitly unattributed rather than asserted to be a compositor failure.

Native regression refs are `data-view-native/**`. They cover admission without
new write authority, XML folding/copy/count/unchanged bytes, a 32 MiB minified file
and a compiled disabled-folding control. Tests record first-draw duration on the
same native runner. These measurements are separate from installed tester acceptance.

The native job also builds the actual app with the non-shipping
`data-view-native-harness` feature and without GPUI test-support. Its real OS
event loop must keep 100 ms heartbeat callbacks below a 2 s gap while opening
XML and both 32 MiB fixtures; an external driver observes the system clipboard
and unchanged fixture hashes. Test-platform draw timings alone are insufficient.

Edit eligibility uses the successful canonical byte load and existing format
authority, never the later AST completion. Native and full Linux checks retain
chosen Edit mode/tree focus, byte-preserving save/conflict recovery, typed viewer
identity across window-mode changes and SVG image-refresh regressions. SVG keeps
its existing image viewer. Probe windows stay alive until every fixture finishes;
closing the final native window is allowed to quit the app.

Toolbar controls/rows are erased into AnyElement at separate construction
boundaries to bound unoptimized native Windows render frames. The real native
probe records its PE stack reserve and trace phases; no stack reserve override
is introduced. It runs before the slower compiled negative/restored controls so
a native startup/render failure is reported first. All controls remain required.

## Shared Find and semantic Tree (#2810)

Tree and paged Raw use a whole-document worker matcher with literal Unicode
case handling and ranges crossing a 32 KiB part boundary. Normal Formatted/Raw
use an editor adapter. It coalesces query/source mutations for 60 ms, snapshots
the Rope and source stamp, and flattens/matches only on a worker. Generation and
source revision checks reject stale results. The toolkit receives prepared
ranges through `set_external_search_results`; it never rebuilds a regex or scans
the source inside an edit callback. Clearing captures the existing match-return
anchor before retiring marks. Query/count/case updates belong to the measured
Find sibling, while cached header slots use static descriptors. The Copy menu
resolves caret-sensitive availability only when opened.

Semantic JSON rows hide index zero, the synthetic document root. Direct children
stay visible after Fold all. `Document` exposes key text, array ordinal, container
kind/count, scalar kind/short source preview, parent and ordered children for
Contents reuse. Keys retain their JSON source escapes. Find tracks only branches
it opened: Esc folds those back except the selected hit's ancestors. A sorted
preorder lookup replaces the old full-index scan; the scrollbar samples no more
than 1,000 hits, including the first and last, with a separate current mark.

The opt-in `code-toolbar-native-app.py` hosts the real Reader on the OS main
stack, uses a synthetic 3.5 MB JSON with 414 paths, and measures synchronous
native CPU draw work (layout/prepaint/paint, excluding compositor/vsync). Opening
paths must finish within 100 ms; all 48 scroll samples per marks state must stay
within 16 ms. This independent step runs even when a baseline native app probe
fails. It changes neither existing event-loop thresholds nor the Windows stack.

The native toolbar gate keeps two distinct profiles: the debug native Reader
smoke exercises Tree rendering and scrolling on the OS main stack; an independent
production release job enforces the 3.5 MB / 414-path CPU budgets (100 ms open,
16 ms per draw, 48 moving frames with marks and 48 without). Both require actual
row builds and viewport movement. A debug smoke receipt cannot satisfy the
release budget runner. Neither gate changes the Windows stack reserve.
