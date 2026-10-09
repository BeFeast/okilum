---
type: Note
---

# Relative Markdown link check

Open this folder as a disposable vault. Start here: `notes/start.md`.

For each case, record separately whether it renders, opens the intended file,
lands at the intended heading/block, and survives save/reopen without rewriting.
Try both a plain click and Ctrl-click (Cmd-click on macOS) in editing views.

A root-level `sibling.md` intentionally collides with `notes/sibling.md`.
M01/M02 must open the note in the source folder.

## Markdown note paths

- M01 explicit same directory: [Sibling](./sibling.md)
- M02 implicit same directory: [Sibling](sibling.md)
- M03 parent directory: [Other target](../other/target.md)
- M04 space encoded: [Space Note](./Space%20Note.md)
- M05 Cyrillic encoded: [Заметка](./%D0%97%D0%B0%D0%BC%D0%B5%D1%82%D0%BA%D0%B0.md)
- M06 cross-note heading: [Target details](../other/target.md#Details)
- M07 same-note heading: [Local heading](#Local%20heading)
- M08 no extension (app-specific note inference): [Other target](../other/target)
- M09 leading slash (app-specific vault-root meaning): [Other target](/other/target.md)

## Attachments: links and embeds are separate cases

- A01 ordinary link to root attachment via parent directory: [Open sample](../attachments/sample.png)
- A02 ordinary link to sibling attachment: [Open local sample](./local.png)
- A03 Tolaria portable attachment convention (vault-root): [Open portable sample](attachments/sample.png)

A04 standard note-relative image embed:

![Blue square: root attachment](../attachments/sample.png)

A05 standard same-directory image embed:

![Blue square: local attachment](./local.png)

## Wikilink and block baselines (app-specific syntax)

- W01 root-relative wikilink: [[other/target|Other target]]
- W02 heading wikilink: [[other/target#Details|Target details]]
- B01 Obsidian block link: [Target block](../other/target.md#^sample-block)
- B02 Obsidian block wikilink: [[other/target#^sample-block|Target block]]

Block references and wikilinks are not part of standard Markdown.
A leading slash is not portable file-relative Markdown semantics.

## Local heading

M07 should land here. This target has a space to exercise percent decoding.
