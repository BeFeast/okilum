# XML edit-worker acceptance and plain refusals

Refs [large edit tools](https://git.oklabs.uk/BeFeast/okilum/issues/2804).
Baseline production: `bff6f8c774bb0b008986d9d0bb945ea9973c65a1`.

The real XML operation worker already preserves complete multi-MB formatting
results. The file host nevertheless wraps complete model refusal messages in
`Not formatted.` and `Nothing was changed.`. Output/depth limits and a mixed
newline refusal therefore appear as three sentences. Preserve these complete
budget/refusal notices at the file host; source/parser/formatter/admission,
Cancel, revision guards, edit transactions and save logic are unchanged.
The single production edit is in `file_chrome.rs::status_text`; no controls,
read-mode tree, Find or Contents implementation changes.

Four new app controls use the real `code_operations` worker and file host:

- A roughly 3.95 MB XML fixture has an independently written full expected
  layout: 3,000 distinct records, mixed prose with meaningful spaces/tabs,
  attribute order/quotes/spacing, predefined/numeric/declared entities, DTD,
  CDATA, comments, processing instructions, xml:space, XHTML prose, Hebrew,
  Cyrillic/Greek, emoji, a combining mark, CRLF and BOM. No XML formatter,
  whitespace stripping or JSON oracle constructs the expected bytes.
- Worker admission is held through its existing gate. Source remains exact
  while the actual host shows busy and Cancel; completion must retire busy.
  Validate is read-only; wide Format must match the complete expected bytes,
  one Undo/Redo must restore exact original/result bytes, canonical save must
  preserve the complete result and must not split that Undo group.
- Valid 9,000-level XML has a 63,000-byte input but cannot fit the existing
  64 MiB indentation output budget. It must report exactly one plain output
  limit sentence, including limit and file size, with no invalidity verdict.
- The XML depth limit and a multi-MB mixed-newline refusal likewise keep source
  and disk unchanged and show one complete plain sentence. Tests inspect the
  actual rendered bottom status, not only a model result or injected string.

On unchanged production the wide-byte/history control passed, while all three
refusal controls failed at the exact rendered-message assertion. This is the
compiled old-production negative; no timeout or compile failure is counted.
Existing JSON host refusal expectations change only where they required the
extra wrapper now being removed. Syntax errors and unavailable formatters retain
separate wording; the legacy-outcome and actual refusal host controls cover that
boundary in addition to the new XML cases.

This establishes functional app-worker/source/host semantics only. It does not
establish native key-to-paint, timing, RSS, every XML vocabulary, platform-native
acceptance or removal of wide Tree::edit/query work. XML rendering/performance,
other long lines, necessary sharing, Parser/Rope, wide traversal and platform
residuals remain owned and unfinished; neither reporter issue is resolved by
these tests alone. No GPUI core/tree-sitter change, private observer, upstream
submission, device action or repeat of a completed native flow is involved.

## Refusal contract follow-up

The JSON file-host control now asserts complete mixed-newline and depth-limit
strings, rather than only a prefix/reason fragment; an extra trailing host
sentence is observable and must fail. A real Repair worker budget refusal gets
the same exact rendered-message/no-preview/no-source-write checks. A separate
admission-notice host control uses the actual model size sentence without
allocating a 64 MiB editor.

Keep `Mixed line endings; source unchanged.` action-neutral deliberately. It
reports a file-state refusal shared by the formatter's callers, and the current
Format command is already the context. It says what prevented the operation and
that source is intact in one sentence; neither an extra action prefix nor a
second host sentence adds a needed decision. Budget messages retain action names
because their limits can differ by operation. This follow-up changes tests/docs
only; it does not fork model/file/fence wording or alter formatter semantics.
