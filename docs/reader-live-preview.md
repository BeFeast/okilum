# Ordinary-note Live Preview — S1 (#359)

This implements the approved MIT S1 from `research/359-smart-editor.md` §9.
Source editing remains the initial mode. While editing, the book glyph beside
Source switches between Source and Live Preview; Ctrl+E / ⌘E retains its existing
Reader/edit behavior. The mode is not restored automatically on the next session.

One EditorState owns authored bytes, selection, composition and Undo. The existing
FileEditor owns save, external conflict and durable drafts. Switching presentation
does not replace text, create an undo transaction or save the note.

The app-owned CachedProvider now lives in `source_presentation.rs`, shared with
managed editing and its isolated native fixture. Classification runs off-thread,
only when Live Preview is requested. A pending job coalesces edits to the latest
snapshot. Adoption checks the editor entity, source stamp and exact text. Stale
results cannot affect another note or a newer generation. Active-block reveal is
still the native provider's responsibility, including selection and IME changes.

Supported presentation remains S1: headings, strong/emphasis/strike, code, ordinary
links and wikilinks. Unsupported blocks remain exact Source. The existing 64 KiB
and 4,096-node limits remain; the glyph tooltip explains whole-note Source fallback.
There is no startup indexing, new dependency, new vendor patch or gpui-core change.

## Acceptance evidence to complete before merge

The managed native matrix in `archive/managed-live-preview-native.md` remains the
acceptance gate, now exercised through the ordinary Reader editor:

| Area | Evidence |
| --- | --- |
| Latest-only adoption and note changes | Native integration regression |
| Byte-exact mode toggles, clipboard, Undo/redo | Integration regression plus real Linux/X11 keyboard, toolbar and xclip; BOM/CRLF/Unicode hashes match |
| Save/conflict/durable draft | Integration regression plus real Linux explicit Save, external rewrite and rejected overwrite |
| Mouse reveal/drag, grapheme arrows, Home/End | Native integration regression; real Linux input capture pending |
| IME | Native UTF-16 bridge regression; real compositor IME preedit/commit/cancel pending |
| Narrow/wide styled wraps and candidate geometry | Existing shared provider foundation; ordinary Reader native acceptance pending |
| Restore/reopen and crash recovery | Real Linux SIGKILL after observing the durable journal, restart in Reader, explicit edit restores the exact draft; external file hash unchanged |
| Large/unsupported notes | Source fallback integration regression |
| Responsiveness | Off-thread classification and coalescing verified; same-host native input/paint timing pending |
| UI rules | Linux light/dark before/after captured on the same fixture/window; compact existing glyph control, no new surfaces or notices |

An API-level IME test is supporting evidence, not a claim of real IME acceptance.
This document must be updated with actual results before merge.

Local validation: 14 Reader editor integration tests and 5 shared provider tests
passed. Shell all-target clippy and fmt passed; shell builds both with and without
Brain (the latter retains pre-existing unused-code warnings). One PR-Agent review
completed. Its Windows concern does not apply: the provider and both consumers
are under parent `cfg(unix)` gates in main.rs; Windows uses reader_editor_windows.rs,
and the header control is inside the existing Unix block. No second review round.

Native lifecycle fixture: BOM + CRLF + Unicode source SHA-256
`f1e11152355b81839ffd6f9ee060ed2dd1d7a1a89aa3f80e06732a6accca17e6`.
Recovered draft hash `9a9465129b839310eb40a782f8f683dd1e4be24993823ee9370fdb2113f992e0`
matched before/after SIGKILL. External disk bytes stayed at
`49f09b06d2387f84f9308d0eb819277a8c104ef48b63ee556d3526391b73b478`.

Publication remains gated on the executor CI slot, #699's save fix and completion
of the pending native acceptance rows. This is not a released-build claim.
