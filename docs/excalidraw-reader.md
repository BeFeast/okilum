# Native Excalidraw reader (#445)

The Reader parses scene JSON directly, including the Obsidian plugin's Drawing
section and LZ-string compressed JSON. It never needs an exported SVG, browser,
JavaScript runtime or external editor to display a drawing.

Use roughr 0.14 (MIT) for sketch paths. Its rectangles, ellipses, curves, polygons,
solid and hachure/cross-hatch fills cover the scene primitives. It exposes vector
operations and accepts per-element seeds. A custom implementation would duplicate
these algorithms without improving the current acceptance target. Its seeded RNG
differs from Rough.js: geometry is stable but individual jitter samples will not
be pixel-identical to the web editor. Compare recognizable geometry, typography,
styles and relationships against the owner's seven drawings.

Okilum owns scene interpretation: arrows, rotation, text placement and fonts,
images, frames and grouping. An in-memory SVG display list carries those vectors
to the native resvg renderer and GPUI image surface. This is generated from the scene, not an export
lookup. Expand must rerasterize vectors at the requested scale and support pan.
No gpui core changes are required. A dedicated resvg font database loads bundled licensed fonts independently of system fonts; gpui core only exposes a fixed bundled-font list.

Parsing is bounded and returns an explicit unavailable view for corrupt input.
Deleted elements do not draw; groups retain scene ordering. Reading a drawing
never rewrites its source.

Editing (#478) writes through `excalidraw::write`, a pure function from the
opened bytes, the editor's post-load baseline and the edited scene to new bytes.
Unchanged elements keep the file's JSON object (so the plugin's `rawText`
survives), and `## Text Elements` / `## Element Links` entries are rewritten only
for elements whose text or link changed, because the plugin reads those entries
as authoritative over the JSON. Deletions are written as tombstones with a
bumped `version`. The Drawing block keeps its encoding; compressed scenes use an
LZ-string encoder that is byte-identical to JS lz-string. Everything else,
including `## Embedded Files`, is copied byte-for-byte, and a save with no change
writes nothing. New text elements with upstream's 21-character ids get no
Markdown entry: the plugin's block refs are 8 characters, and it adopts such
elements from the JSON on load.

## Edit drawing (#478)

Edit drawing (pencil on an embedded drawing, Open menu of a drawing file, the
Edit drawing menu command) opens the drawing in an offline Excalidraw page that
Okilum serves itself. Displaying a drawing never starts it, and nothing goes to a
remote editor, storage backend or the clipboard; Copy scene stays in the Copy
menu as a separate command.

- **Page.** `web/excalidraw-editor/` holds the source: React plus
  `@excalidraw/excalidraw` 0.18.1, pinned by `package-lock.json`, bundled with
  esbuild into `crates/okilum-shell/assets/excalidraw-editor/` together with the
  package's fonts. The output is committed and embedded in the binary
  (about 16 MB; JavaScript and CSS gzip-compressed), so a desktop build needs no
  Node. `node build.mjs --check` rebuilds and compares byte for byte; the
  nightly `forgejo-excalidraw` job runs it, and a shell test checks that the
  embedded bytes match `SHA256SUMS`.
- **Bridge** (`crates/okilum-shell/src/reader_drawing_edit.rs`). One session per
  drawing, holding the drawing's `DrawingEdit` (okilum-core
  `excalidraw::edit`), so the editor lock, recovery draft and revision check are
  the ones notes use. It listens on `127.0.0.1` on an OS-chosen port. Every URL
  is under a random 256-bit capability; other paths get 404. `Host` must be the
  listener's own address (no DNS rebinding), and a change needs the page's
  exact `Origin` and a JSON body. Requests are bounded (16 KiB headers, twice
  the drawing size limit for bodies, 15 s I/O). The page's CSP allows only its
  own origin, which also blocks Excalidraw's CDN font fallback.
- **Apply.** The page reports the scene it holds right after loading, so load
  normalisation is not an edit; Apply sends the scene and Okilum writes the
  lossless writer's bytes. Undo Apply restores the exact previous bytes once.
  If the file changed outside, nothing is written and the page offers Reload,
  Keep mine or Save as copy. Keep mine names the outside version the page was
  shown (by SHA-256); if the file changed again, that newer version is reported
  instead of overwritten. Done is refused until such a conflict is resolved.
- **Lifetime.** Done ends the session and releases the lock; so do two idle
  hours and quitting Okilum. Closing the tab without Apply leaves the drawing as
  it was. A second Edit of the same drawing opens the same session; a second tab
  asks before taking it over, after which the first tab's revision is stale.

Line endings are kept as opened, so a CRLF drawing is saved as CRLF. Obsidian
reads such files correctly (#938): `vault.read` returns the CR bytes, but the
text handed to the plugin's view arrives with LF endings, and the plugin's loader
only ever parses LF. Checked with Obsidian 1.12.7 and 1.14.4 and plugin 2.28.1 on
a CRLF writer fixture. The CRLF drawing loads with the same elements and text as
its LF twin, and a corrupted control does not load. The CI plugin oracle
normalises CRLF before `loadData` for the same reason.

Decoded scenes use a 16-entry LRU. Raster variants share a 128 MiB / 64-entry
LRU budget (excluding currently displayed images and in-flight renderer allocations).
Vault watcher events invalidate decoded scenes, including external image dependencies.
Rasterization caps each side at 4096 pixels; further zoom enlarges that raster.

Okilum serializes roughr operation lists itself: roughr 0.14 emits `L` for
`Move` in its SVG helper. Pixel tests cover solid and patterned fills with no
outline, preventing a broken path encoder from passing on text alone. Nunito
subsets carry an internal ExtraLight family name and receive a fontdb alias.
