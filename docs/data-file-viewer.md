# Read-only data files (#2669)

## CSV and JSON tables (#2735, decision #1532)

CSV/TSV and JSON arrays open as read-only tables. JSON retains its Source view
(Pretty/Raw, Find, folding and exact Copy); JSON objects/configuration files
continue to open as source. JSONC and JSONL/log routing remain unchanged.
Objects in an array use the union of their top-level keys, nested values stay
compact JSON, missing fields stay empty and explicit null displays `null`.
Arrays of arrays use numbered columns; scalar arrays use a Value column.

Click a column heading for ascending, descending, then source order. Numeric
columns sort numerically; other columns use case-sensitive text order. Filter
loaded rows is a case-insensitive substring across every cell. Sorting and
filtering run on workers; obsolete results cannot replace newer queries. Copy
uses displayed row order and clears the selection when the projection changes.
Both rows and columns are virtualized. The no-header CSV option retains every
source row and provides numbered sortable column headings.

The first preview is bounded by 1 MiB, 1,000 displayed rows, 64,000 cells,
256 columns and 64 KiB per field. Load more rows explicitly re-reads up to
32 MiB, 100,000 rows and 500,000 cells with the same column/field bounds.
Counts identify the loaded scope and partial files. Filters and sorts cover
only that scope; the limit notice never claims the whole file was searched.
Cut records are omitted rather than displayed as complete values. Oversized,
malformed or unsupported JSON still has the Source view.

Open in DuckDB starts an installed DuckDB CLI using its documented
`-cmd "CREATE VIEW ..." -ui` interface, exposing the full canonical file as
`okilum_data` in an independent in-memory database. CLI arguments bypass the
shell; SQL literals and file glob characters are escaped. The CLI process is
reaped independently of the Reader. Missing/failed CLI launches display an
inline retryable error. Installation and extension downloads belong to the
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
The external editor button uses the existing guarded default-app file action.

Files up to 64 MiB load on a worker with a bounded read even when the file grows.
Parsing, JSON Pretty and full-file search run on workers. Code formats above their old
4 MiB edit budget remain read-only. TXT retains its pre-existing explicit editor
and canonical FileEditor checks; the Read view is always read-only. Large multi-line files start without soft
wrap; the existing wrap preference continues for smaller files.

A line longer than 64 KiB uses 32 KiB UTF-8-safe pages, with explicit page and
byte ranges, Previous/Next and whole-file Find with total count and navigation.
Copy selection is exact within the displayed page; Copy node can cross any number
of pages. Search can match across a page boundary. A spanning match selects its
part on the current page. Gutter numbers in the explicitly paged Source view are local to the page.
Structured long-line files initially show a virtualized Structure view over the
complete AST, with original source line numbers, individual node folding, Fold
all/Unfold all, exact whole-node Copy and Source navigation. Tree folding is
independent of page boundaries, and row labels read at most 96 characters.
Find navigates to the matching Source page. No placeholder text is inserted into
the source or copied as file contents. JSON Pretty can also restore the complete
multi-line code view without changing its file.

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
