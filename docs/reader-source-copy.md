# Reader Source copy (#65)

The default Markdown Reader's select-all Source copy must return the original
displayed note body before reader transformations. Leading YAML frontmatter stays
excluded. Body whitespace, LF/CRLF, Unicode, authored HTML, highlight markers,
wikilinks, image paths and unexpanded embeds remain exact valid UTF-8 text.

`render::reader_document` obtains `rendered` and `original_body` from one primary
file read. Embedded notes are expanded only in `rendered`; the original body's
embed syntax remains unchanged. The existing `reader_source` API returns the same
rendered input through this snapshot API. Heading navigation continues to use
rendered input.

The shell/vendor integration must attach original-body metadata to the accepted
parsed replacement revision, preserving the old pair while an asynchronous parse
is pending. Identical rendered input with different authored spelling must update
the metadata. Unpaired text replacement or append invalidates it; extension-only
reparse retains it. Exact select-all Source copy bypasses clipboard trimming and
the shared selection join so whitespace-only bodies and terminal newlines survive.

This contract does not add an inverse source map for partial selection through
transforms or expanded embeds. HTML remains Plain. Reader rendering and the editor,
SourceWrite and MCP APIs keep their existing roles.

Core tests cover original body bytes across transformations, frontmatter and both
path forms, equal-rendered reloads, whitespace-only content and read errors. Final
acceptance additionally requires actual native clipboard bytes, async replacement,
search navigation and Custom drag-selection evidence; a selected-text assertion
alone does not establish clipboard correctness.
