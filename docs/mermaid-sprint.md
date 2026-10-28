# Mermaid Sprint, 28 October 2026

Manager-authorized Sprint delivery for #1739 (duplicate of technical record
#1182). This branch pins `merman = "=0.9.2"`. The later main PR still requires
Oleg's acceptance and the existing ≥30/33 visual gate. No corpus is committed.
CT141 `/tmp/okilum-mermaid-corpus` was absent; it is not reconstructed.

Reader and Live Preview share the Markdown extension. In LP only Mermaid code
fences join S7 blocks: caret/selection/composition reveals their source through
the existing projection contract. Other code fences keep their presentation.

Rendering uses `SvgPipeline::resvg_safe()`, the dangling drop-shadow reference
repair and embedded Apache-2.0 Arimo 1.23. A dedicated resvg font database is
independent of installed fonts. Merman's text-width profile uses rustybuzz and
those same embedded faces for layout as resvg uses for painting; heuristic
32px label viewports previously clipped wider actual glyphs. Diagrams follow
light/dark mode. External image
references are rejected rather than loaded from local files or the network.

Two worker threads and a bounded queue contain `catch_unwind`; a five-second
wall deadline covers queueing, parsing, layout and rasterization. Merman also
receives cooperative cancellation/deadline control. Inputs are at most 64 KiB,
SVG 4 MiB, raster 4 megapixels/4096 pixels per axis. A stuck worker cannot spawn
replacement threads without bound.

The cache uses source SHA-256, 128 CSS-pixel width buckets and theme. At most 32
entries/64 MiB of rasters are retained, with matching GPUI asset eviction. A
block retains its previous good image while its replacement is pending. Parse,
render, resource-limit, panic and timeout errors display the original highlighted
Mermaid fence, never an empty placeholder. Source and code-copy remain exact.

The default merman SVG closure includes its EPL-2.0 ELK implementation. Its pinned
source/provenance is available at https://crates.io/crates/merman-elk-layered/0.9.2
and https://github.com/Latias94/merman. Arimo provenance and its Apache-2.0 notice
are in `crates/okilum-shell/assets/mermaid/`.
