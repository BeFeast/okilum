# Read-mode context menus (#2025)

The text/background slice adds **Copy** (plain text) and **Copy as Markdown**
for a selection. Without a selection it offers **Select all** and **Copy entire
document as Markdown**. Heading links and date reminders remain in a separate
group. Source and Live Preview editing menus are unchanged.

Copy payloads are captured when the menu opens, without changing the selection,
scroll position or default copy format. Whole-document Markdown uses the exact
original body paired with the renderer's accepted revision, including whitespace
and LF/CRLF; leading YAML frontmatter stays excluded as in [Source copy](reader-source-copy.md).
An asynchronous replacement keeps the previous displayed document's source until
its replacement is accepted. A parse failure disables whole-document copy.
Partial Markdown selection uses the existing renderer source mapping and retains
its documented limitations around transformed syntax and expanded embeds.

A right-click inside a code block also offers **Copy code** with the same bytes
as the block Copy button. Links offer **Copy link address** (resolved vault or
heading links use a full Okilum address) and **Copy link text**; selection formats
join that same popup when text is selected. Targets and copy payloads are captured
for the press and accepted document generation.

This is a partial implementation of #2025. Rich HTML selection, image address/Save as,
and composition of selection groups with image/table menus remain follow-up slices. Existing image commands,
table Markdown/TSV menus and the code-block Copy button are retained.

## Verification

- Pure command policy: compile `reader_read_menu/commands.rs` as a module in a
  `rustc --test` harness (no Cargo dependencies).
- Shell mock regression: `background_and_selection_menu_write_captured_clipboard_bytes`.
- Vendor mock regressions: `explicit_copy_formats_preserve_preference_selection_and_original_body`
  and `exact_source_tracks_displayed_async_revision_and_discards_late_results`.
- Vendor snapshot API: `0111-read-copy-snapshots.diff`; object press API reused from
  #2151: `0113-read-object-presses.diff`. gpui core is untouched.

The mock regressions require the normal prepared vendor tree and a focused shell /
vendor test build. Native clipboard and GUI acceptance remain separate evidence.
