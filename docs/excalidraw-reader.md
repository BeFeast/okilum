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
elements from the JSON on load. The explicit Open in Excalidraw action copies the scene
JSON and opens the owner-approved https://draw.oklabs.uk editor; it is not part of normal rendering.

Decoded scenes use a 16-entry LRU. Raster variants share a 128 MiB / 64-entry
LRU budget (excluding currently displayed images and in-flight renderer allocations).
Vault watcher events invalidate decoded scenes, including external image dependencies.
Rasterization caps each side at 4096 pixels; further zoom enlarges that raster.

Okilum serializes roughr operation lists itself: roughr 0.14 emits `L` for
`Move` in its SVG helper. Pixel tests cover solid and patterned fills with no
outline, preventing a broken path encoder from passing on text alone. Nunito
subsets carry an internal ExtraLight family name and receive a fontdb alias.
