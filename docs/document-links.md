# Consistent document links

Issue #314 separates ordinary Markdown document destinations from wiki identity.
Comrak supplies inline, reference, title, angle and wiki syntax with ranges into
unchanged authored bytes. When strict parsing rejects an inline destination with
spaces, an existing-file fallback recognizes Obsidian paths (including literal
`~md~`); code, HTML, escaped syntax and image labels remain excluded. Percent
encoding is decoded once, and angle destinations retain CommonMark semantics.
The same parser/resolver drives Reader clicks, Linked from and rename previews.
The core structured result drives source/HTML rendering,
preview metadata and explicit Source/Live Preview actions. Conservative Source
presentation does not change link meaning. Code spans and blocks are excluded.

Markdown `.md` paths are source-relative first. Explicit `./` and `../` are strict;
leading `/` denotes vault-root unless it identifies an absolute filesystem path.
Absolute paths inside the open vault become vault-relative; existing files outside
the vault offer explicit Reveal/Copy actions and never open automatically. Only absence allows root-exact and unique-suffix
compatibility fallback. Existing invalid/unreadable/dangling entries at either
exact location cannot redirect to a suffix namesake. Multiple local casefold
candidates are ambiguous. Wiki root-exact/shortest-unique behavior is preserved.
Split the raw fragment delimiter before decoding path and fragment once; escape
transport delimiters independently so literal percent/hash filenames round-trip.

Heading text uses case-insensitive, whitespace-normalized matching, not GitHub
slugs. The shared Comrak inventory rejects missing and duplicate headings before
surface-specific landing. Reader supports top-level ATX and Setext; managed Source
retains its 64 KiB saved-target and accepted ATX limits. Unsupported containers,
blocks and stale replies cannot land successfully at note top. Self-heading links
use the same guarded navigation and Back path as cross-note links. Block references
(`note#^id`, #651) resolve like headings against the Reader's block-ID markers:
a missing or duplicated ID refuses visibly; managed Source reports them unsupported.
See [Obsidian syntax](obsidian-syntax.md).

Preview replies add `document_links_version: 1`, `wiki`, `heading` and `reason`.
`target` retains its legacy base-only join meaning; the additive `authored_target`
retains the complete parser destination including fragment. Heading-bearing rows
use `resolved_heading` / `ambiguous_heading` statuses: older clients visibly refuse
them instead of opening a document while silently ignoring heading intent. Emitted URLs exactly
match metadata; ambiguous Markdown and wiki URLs have distinct grammar identity.
Conflicting rows with one URL refuse visibly. Older backends lack this capability
and refuse navigation without changing source. Resolved candidates remain subject
to source-store readability and ownership checks.

Reader/rendered-preview clicks navigate; ordinary Live Preview clicks edit.
Explicit Open link uses the shared parsed source, including reference links in
blocks conservatively displayed as Source. Existing dirty, recovery, IME,
source-size, revision and navigation-owner guards remain authoritative. External
HTTP(S) works; unsupported URI and extensionless Markdown links report scope.
Local attachment links open an in-app file preview (#388), using source-relative
then vault-root resolution for Markdown and exact-root then unique-suffix
resolution for wikilinks. Ambiguous targets require a choice. Opening an external
application or Quick Look is an explicit file action; link activation alone never
launches an external application.
No source normalization or automatic conversion is performed.

The portable synthetic corpus lives under
`crates/tessera-core/tests/fixtures/document-links/`. Regression checks include
root/sibling sentinels, exact fragments/ranges, encoding once, code exclusion,
wiki compatibility, fallback boundaries, heading ambiguity and actual managed
navigation/Back. Native acceptance records exact binary/source/vendor, host,
gestures, destination, heading, refusal, Back and source-byte preservation
separately from headless test results. Managed image attachment actions remain
follow-up #315.

## Incoming references (#316)

Reader Linked from and managed incoming references include ordinary Markdown
note links, using the same source-relative resolution as navigation. Inline and
reference forms, encoded filenames and heading-qualified destinations contribute
note edges; code examples, malformed links and attachments do not. Wiki behavior,
including embeds and explicitly ambiguous candidate rows, remains unchanged.
Managed rows retain original source lines, revisions, scope filtering, pagination,
budgets and per-source/target/line deduplication. Self-links are excluded.

Managed Markdown resolution uses backend metadata, but only accepted indexed
snapshots receive edges. Resolver discovery does not enroll additional content or
expose operational records. Derived cache v6 rejects wiki-only generations and
binds reuse to metadata identity, including occupied non-note paths that block
fallback to a root namesake. Rebuild changes no canonical files.

## Prepared link presentation (#336)

Reader and managed rendered preview retain each authored label as a link. The
preparation result distinguishes resolved, missing document, missing heading,
ambiguous, unsupported and unknown/unavailable. Missing links have a contrasting
wavy underline and explanatory hover text. Their clicks are inert: no navigation,
history entry or redundant notice. Unknown links explain unavailable evidence;
external URLs are never probed.

`HeadingInventory` is the shared Comrak inventory for Reader navigation, link
preparation and TOC consumers. Build it from the actual prepared source revision.
It retains Setext and unsupported-container entries, counts duplicate matches and
supplies block/byte locators. Managed preparation also applies the existing Source
classifier's accepted syntax and size limits; it does not narrow Reader support.

Reader Markdown preparation and target verification run on background jobs. Each
job reuses one resolution per source/grammar/target and one source read/inventory
per target. `ReaderDocument.links` binds authored interpretation, originating note
(including embeds), and the exact emitted URL. Refresh updates status/action for
that identity rather than rewriting displayed text or reconstructing the old URL
from a changed filesystem. Root/document and refresh generations reject late
results; pending or incomplete inventory cannot prove absence. The existing
watcher refresh seam recomputes status after appearance, deletion, rename or
heading edits. Startup vault scans and index work are separately tracked in #338.

During same-source Reader revalidation, the last verified link appearance remains
visible while action evidence becomes pending. A duplicate save/watcher refresh
cannot temporarily turn a known missing link into an ordinary link (#482).
Accepted results refresh document and table-overlay paint without replacing text,
selection or scrolling. Accepting another source/document clears that appearance;
late jobs cannot restore the previous document's evidence. Cmd+E still performs
the approved lifecycle save before returning from Source to Reader.

Managed preview adds `prepared_links_version: 1` and a `prepared` object on each
link row, including target revision. Existing preview path, source revision,
draft digest, workspace scope and generation guards remain authoritative. Target
bytes use the existing scoped source store; backend paths never become desktop
paths. Workspace Refresh also refreshes link evidence. Offline/older evidence is
unknown, with normal unavailable-operation feedback rather than a missing style.

Vendor patch `0018-text-link-presentation.diff` exposes a small `TextView` link
presentation hook inherited by nested parts. It preserves text runs and canonical
Source copying, and supplies per-link hover/inert handling without GPUI core
changes. Known-missing single clicks register an exact clipped link hit test with
patch 0019's shared per-frame selection-preservation controller before Root
capture can clear an existing range. MouseUp still reaches shared cleanup;
ordinary text clicks and multi-click selection retain their behavior. Cross-note
heading inventories use the same heading-relevant Reader source transforms,
including highlights, as the actual destination view.

Upstream submission and native hover/click acceptance remain separate
review gates; widget evidence does not qualify a native platform build.

## Adjacent image attachments (#315)

Ordinary image links and embeds use source-relative identity with one percent
decode. Explicit `./` and `../` do not redirect. Bare `attachments/...` retains
root compatibility only when its source-relative candidate is absent; an existing
invalid entry, including a dangling symlink, blocks fallback. Ambiguity is visible.
Reader keeps its existing file preview and cached image first paint; completed
image preparation uses the same bounded resolver as link actions.

Managed preview adds `attachment_links_version: 1` and per-link `asset_url` plus
`asset_revision`. PNG, JPEG, GIF, WebP, SVG and BMP bytes travel through the existing
scoped source store and opaque asset table with unchanged byte limits. Ordinary
links and matching embeds share those bytes. A click or explicit Source/Live
Preview Open link shows the image in the preview pane; the back glyph restores
the note. Changing preview ownership clears the image. Backend paths never become
desktop file paths or OS-launch requests. Unsupported, unreadable or ambiguous
images give an explicit reason. Old replies without the attachment capability
cannot open an image. Sources remain unchanged.
