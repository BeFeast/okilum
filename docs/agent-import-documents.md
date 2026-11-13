# Agent document imports (#1588)

With Agents enabled, instruction documents expose standalone `@path` directives
as links with a read-only disclosure immediately beneath the directive. Reader
and Live Preview use the same decoration renderer. Source retains exact bytes
and follows a directive with Cmd/Ctrl+click. Entering a project does not remove
these note features; the Board remains separate.

`agent_imports::is_agent_document` is a document classifier. It includes the
vault-root `@AgentContract.md` independently of the conventional names used by
`quick_open::is_agent_file` for tree/search filtering. Adding a document feature
must not silently change which files the browser hides.

`reader_document_decorations::install` is the shared document decoration hook.
It preserves ordinary Markdown parts and binds imported documents to their own
canonical source path. Imported Markdown is prepared on the existing background
worker through `reader_document_from_source`, so links resolve relative to the
imported file. Nested directives use the bounded expansion tree; failures stay
visible at the directive. Expanded content never enters the editable source.

Directive identities and disclosure state belong to the displayed snapshot.
Source edits retire old byte ranges; navigation validates the current snapshot
token and re-resolves the target through `RootedSource` before opening it. Live
Preview receives directive ranges from its background classification, draws
them as document blocks only for agent documents, and exposes exact source while
the caret/selection/composition touches the directive. Unclassified edits cannot
reuse earlier directive offsets. No gpui core or vendor changes are needed.
