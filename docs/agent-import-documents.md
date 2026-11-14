# Agent document imports (#1588)

With Agents enabled, instruction documents expose standalone `@path` directives
as links with a read-only disclosure immediately beneath the directive. Reader
and Live Preview use the same decoration renderer. Source retains exact bytes
and follows a directive with Cmd/Ctrl+click. Entering a project does not remove
these note features; the Board remains separate.

`agent_imports::is_agent_document` is the agent document classifier. It adds the
vault-root `@AgentContract.md` to the conventional names of
`quick_open::is_agent_file`. Document features and the search file filter
(Quick Open, content search, Pinned and Recent; #2940) use it; tree visibility
keeps the filename rule. Adding a document feature must not silently change
which files the browser hides.

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

## `@@name` is not an import (#2939)

Claude Code imports a file whose name starts with `@` only as `@./@name`,
`@~/@name` or `@/…`; it silently ignores a line `@@name`. Okilum follows what an
agent actually loads: `agent_imports::parse` never returns such a line, so it is
not a link, never expands, adds no Rules layer and counts no bytes or tokens.
`agent_imports::ignored` returns these lines instead. Reader and Live Preview
draw them as plain text with the hint from `agent_imports::ignored_message`
("Not an import: Claude Code ignores `@@AgentContract.md`. Write
`@./@AgentContract.md`."), the Agent file panel lists it with its line, and the Rules tab lists the same hint as a diagnostic
of the file and line that contain it.
