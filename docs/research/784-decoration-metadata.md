# Live Preview decoration metadata (#784)

This slice adds immutable marker metadata only. It does not enable decorations,
change projection/reveal policy, or claim native visual acceptance.

`Classification::decorations_for` requires the exact classified `Snapshot`,
including document, generation and source bytes. Stale metadata is rejected;
there is no retained/remapped decoration inventory. The extractor consumes the
same Comrak AST after the classifier's existing guards. It does not add a parse,
change formatting traversal, or modify Plan/Region, styles, links, reasons or
marker_scopes.

The inventory contains unordered bullet delimiters and nesting depths, explicit
quote delimiters with AST container scopes, and AST thematic-break ranges.
Ordered items and checkbox prefixes retain their current appearance. Frontmatter,
setext underlines and fenced-code contents are distinguished by AST node type.
The classifier's configured frontmatter ambiguity remains unchanged.

Raw delimiters are checked against AST source positions. Unsupported coordinates,
ambiguous prefixes, overlapping ranges or metadata limits discard the complete
inventory, without changing existing classification results. The initial quote
prefix extraction supports spaces and nested `>` prefixes; a quote beginning
behind a list marker is conservatively raw. It does not invent ranges for lazy
continuation lines; their geometry must be derived from the AST container scope
by the later paint adapter.

Integration remains gated on editing-executor review of the shared reveal and
mouse gesture epoch adapter. Foreground suppression is a separate review spike
using existing `ShapedLine::split_at`, never a shaping call. No spike code is
wired into the editor by this slice. Before enabling paint, require same-prepaint
LayoutStamp validation, raw fallback on invalid geometry, and native light/dark
and narrow-width evidence with unchanged row geometry and anchor delta Y = 0.
Byte-identical save/Undo and IME/selection acceptance remain pending integration.

Local validation on CT141: 28 source-classifier unit tests passed, including
six new metadata tests. The on/off case checks identical plans, styles, reasons,
rendered projection text and source-to-display maps across caret positions.
This is core metadata evidence only, not native paint or save/Undo acceptance.
