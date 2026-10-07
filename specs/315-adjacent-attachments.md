# Adjacent image attachments (#315)

Ordinary image attachment links and Markdown image embeds share source-relative
identity, encoding and root boundaries. Supported formats: png/jpg/jpeg/gif/webp/
svg/bmp. Preserve relative-first, then root compatibility for `attachments/...`;
explicit `./` and `../` never redirect. Existing invalid entries block fallback.
Do not rewrite notes or change external fetch policy, document/wiki navigation,
PDF/audio/video support or OS file actions.

Reader retains its existing file preview. Managed rendered preview and explicit
Source/Live Preview Open link show the supplied image bytes in the preview pane;
a single back glyph returns to the note. No modal or filesystem handoff. Preview
ownership, source revision, asset revision and existing byte limits remain bound.
Missing, unsupported, unreadable and ambiguous attachments report their outcome.

Validate exact root/sibling and parent-relative paths, encoded Unicode filenames,
corresponding embeds, missing/invalid targets, bytes and stale preview actions.
One GLM review, affected fmt/clippy, CI, Linux beta. Apply all UI rules; Linux
light/dark before/after evidence must be collected without using muninn's shared
screen outside its exclusive QA sub-session.

UI rules checklist: the new action stays in the existing preview pane (1), makes
no source change requiring Undo (2), and adds no notification strip (3). Return
uses a ghost arrow with tooltip (4), existing control tokens (5), no additional
frame (6), the image title without path/extension (7), application typography
(8), and a platform-neutral tooltip (9). The compact inline preview follows the
existing document pane rather than introducing a dialog (10). Surrounding legacy
Brain controls are unchanged. Evidence uses the actual native Linux application
on an isolated software-rendered X11 display; it is not GPU/performance evidence
or a replacement for the manager-owned native acceptance pass.
