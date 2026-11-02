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

This is a partial implementation of #2025. Table selection-group composition remains a follow-up slice. Existing table Markdown/TSV menus and the code-block Copy button are retained.

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

## Rich HTML copy

For native Markdown Read selections, **Copy with formatting** offers standard
HTML plus the captured plain-text alternative. It shares the text/background and
link popup, preserving the existing plain and Markdown actions. Both inputs come
from the accepted selection captured when the menu opens; parsing never reloads
the note or resolves against a later document revision. Legacy HTML-renderer mode
keeps this action disabled rather than interpreting its plain fallback as Markdown.

HTML conversion runs off the UI thread, preserves code whitespace and inline
marks, and omits raw active HTML. The selection limit is 1 MiB Markdown and the
combined native offer limit is 8 MiB. Plain text is copied immediately. A unique
clipboard-offer marker prevents late conversion from overwriting any newer copy,
including one with identical plain text. Conversion/native writer failures leave
plain text and show the failure; they do not claim formatted-copy success.

GPUI's string metadata is not an HTML MIME representation. The app-owned adapter
uses pinned `arboard` to offer HTML and plain together (X11/Wayland data-control,
Windows HTML Format/Unicode text, macOS HTML/string), retaining the native owner
for the app lifetime. Unsupported native clipboard environments fail visibly with
plain text retained. No GPUI core patch or clipboard subprocess is introduced.

Focused mock clipboard checks exercise the real Read and link menu actions,
accepted-revision capture, plain/Markdown preservation, offer cancellation and
writer refusal. These checks do not establish native desktop interoperability.

## Image address and Save As

The existing image popup offers **Copy image address** for the exact resolved
file URL or HTTP(S) URL. It never fetches an image. Native Markdown Read also
offers **Save image as…**, preserving the original bytes, metadata and animation
rather than applying Copy image's PNG conversion. Selection formats join this
same popup when a selection exists. Source/Live Preview keep their existing
image actions plus address copy; Save As requires an accepted Read source.

Save As captures the owning note, navigation generation, accepted canonical
source and resolved image. A local file revision is captured before the picker
and checked before/after reading and before publication. A changed accepted
render, navigation or image discards the completion. Canceling the picker is a
no-op: no byte read, temporary copy or HTTP request. Remote Save As downloads
only after explicit destination selection, using the existing Copy image fetch
policy (HTTP(S), 15 s, 50 MiB, five redirects, image response validation). It saves
those response bytes, not a promise that a mutable server returned the same
response previously displayed. Local saves are bounded at 64 MiB. Missing,
ambiguous, outside-vault and unmaterialized iCloud images cannot be saved.

Destination publication reserves the existing editor/recovery lock, stages and
flushes bytes off the UI thread, then validates the source lease before atomic
create-only publication. Existing files (including the original image, symlinks
and concurrent creators) are never replaced, even if the OS picker confirmed
Replace; choose a new filename instead. No source/draft/history is overwritten.
Unix uses the existing descriptor-anchored exclusive publication primitive and
folder sync; Windows uses the ordinary local-NTFS directory pins and native
write-through no-replace rename. Platform restrictions/failures remain visible.
A folder-sync failure reports that the copy may already exist; inspect it before
retrying. No Save As action changes the note or its Undo history.
