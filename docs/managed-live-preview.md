# Managed-note Live Preview — bounded first slice

Status: reviewed contract for [P1 issue #211](https://git.oklabs.uk/BeFeast/okilum/issues/211).
The mapping/classifier and native adapter are accepted through #216/#218/#219.
Managed integration is implemented in #220; its integrated native acceptance is
tracked separately from source tests. See [integration](managed-live-preview-integration.md).

[The POC](ai-brain-poc.md) accepts separate Source and rendered preview and names
Live Preview as an eventual requirement. This slice adds an optional editing mode
for managed notes. The v0 Reader remains read-only; this is not complete Obsidian
editing parity or a new backend protocol.

## Visible behavior

The editor offers **Source** and **Live Preview** for the same document, with one
scroll surface, focus, caret, selection and undo history. In Live Preview, the
active paragraph/block and all blocks intersected by selection or IME composition
show their authored Markdown markers. Supported inactive text displays its
formatting and link label. Clicking text positions the caret in that exact source
block and reveals its markers without changing the document.

The initial set is paragraphs, ATX headings, emphasis (bold, italic and strike),
inline code, ordinary links and wikilinks. Frontmatter, fenced blocks, tables,
embeds and other unsupported constructs remain exact editable Source in place.
An incomplete or ambiguous construct also remains Source. Ordinary click edits a
link; following a link requires a deliberate existing link action/modifier, so
positioning the caret never navigates away from an unsaved draft.

## Canonical source and editing boundaries

- One complete canonical UTF-8 source buffer owns all edits. Rendering is a
  discardable projection bound to its exact document generation. Preserve BOM,
  CRLF/LF, absent final newline, whitespace, escapes and unsupported syntax.
- Parse authored source directly into source ranges. The transformed
  `source_preview`/Reader output is never editable canonical input and must not be
  reconstructed back into Markdown. Stale parse/projection results cannot replace
  the current generation.
- Source ranges use UTF-8 byte coordinates; caret movement respects grapheme and
  existing CRLF boundaries. Native IME still uses the existing UTF-16 bridge.
  Define both source-to-display and display-to-source boundary bias explicitly;
  a hidden delimiter must never create an ambiguous destructive edit target.
- Reveal every block intersected by selection or IME before editing it. Copy,
  cut, paste, deletion and multiline selection operate on exact canonical source
  spans, including revealed delimiters. The projection never creates a separate
  rendered-text editing clipboard or per-block editor buffer.
- Projection updates, marker reveal, theme/layout changes and mode switches create
  no edit transaction and do not call `set_value`, which resets caret and history.
  Typing, paste and deliberate Markdown edits use the existing UndoManager. One
  undo/redo chain survives reveal and Source/Live Preview switches.
- Retain existing explicit document-load, recovery-restore and acknowledged
  automatic-merge adoption history-reset boundaries. Ordinary Save retains edit
  history. Persisting undo across process restart is outside this slice; exact
  source drafts and pending requests remain recoverable.

## Implementation seams

Application-owned parsing produces bounded, generation-bound source ranges and
styles. The likely native seam is `InputBaseState<EditorMode>`/`EditorState` with a
small conceal/reveal projection layer before the existing WrapMap/DisplayMap.
The current InputHighlighter supplies styles and line folds, not an inline source
projection; TextView's rendered selection offsets are not canonical source ranges.
Do not label either existing capability as sufficient without proving the missing
mapping behavior.

Keep the generic projection primitive in a small reviewable gpui-kit patch if that
route passes its acceptance cases. Do not patch gpui core or introduce per-block
Textarea instances. If the primitive cannot preserve native caret/IME/undo, report
that approach as failed before wiring it into product editing.

`BrainView` retains source loading, explicit Save, navigation and conflict ownership.
The adapter exposes the same full source value and change signal to
`editor_recovery`/`editor_ui`. Existing SourceWrite CAS, pending exact envelopes,
local v1/v2 recovery, #206 true-base protection and deliberate manual resolution
remain authoritative. Presentation changes never save source, advance a loaded
revision, recompute a pending child or retire a recovery record.

## Staged acceptance

1. **Projection primitive:** deterministic mapping and edit tests cover supported
   syntax, malformed/unsupported source, Unicode/graphemes, BOM/CRLF/final newline,
   hidden-boundary bias and exact source preservation. A native fixture proves
   click, arrows, Home/End, Shift selection, drag, multiline paste/delete, IME and
   undo/redo across reveal. Passing a parser or style demo does not pass this stage.
2. **Native single-buffer adapter:** switching Source/Live Preview preserves source
   bytes, selection, focus and undo. Supported paragraphs can be written in one
   surface; unsupported blocks remain editable Source. A stale parse cannot move
   the caret or overwrite newer text. Typing never waits synchronously for parsing;
   report same-host native input/paint measurements on an identified bounded corpus.
3. **Managed integration:** existing exact Save, conflict, close/navigation,
   crash/reopen and pending-request recovery work through the adapter. Include a
   #206 late receipt: newer text retains its true base and ordinary Save remains
   blocked when resolution is required. Reopening verifies exact retained bytes.

Success is observable in the real native editor: write and revise a supported
Markdown note without a second preview pane, switch modes without losing position
or undo, copy exact authored selections, and recover exact bytes after reopening.
Default activation or wider construct support requires the corresponding native
acceptance evidence; this contract authorizes no claim that those outcomes exist.
