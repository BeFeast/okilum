# Read-only data files (#2669)

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
