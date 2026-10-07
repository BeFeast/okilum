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
| Mouse reveal/drag, grapheme arrows, Home/End | Native integration regression; real X11 Left through combining/ZWJ/flag graphemes, Home/End and forward/reverse cross-block drag passed; full wrapped-row matrix remains pending |
| IME | Native UTF-16 bridge regression; real compositor IME preedit/commit/cancel pending |
| Narrow/wide styled wraps and candidate geometry | Actual X11 click after inline code/bold at 1,240 and 900 px returns canonical W offset 4 after reveal, checked through external clipboard; IME candidate geometry remains pending |
| Restore/reopen and crash recovery | Real Linux SIGKILL after observing the durable journal, restart in Reader, explicit edit restores the exact draft; external file hash unchanged |
| Large/unsupported notes | Source fallback integration regression |
| Responsiveness | Same-host Xvfb input-to-framebuffer probe below; actual compositor acceptance remains pending |
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

### Additional native geometry and timing evidence (2026-10-07)

The geometry fixture uses inline code followed by bold `WWWWWW` and a long
wrapping paragraph. At both 1,240 and 900 px window widths, a click on the fourth
painted W, followed by selection to document start, copies exactly the canonical
prefix ending at W offset 4 after reveal. A sentinel replaces the external
clipboard before each copy, preventing old clipboard bytes from passing the
check. Neither run changed the fixture file. Narrow windows hide the sidebars;
the probe uses their actual editor and toolbar positions.

A paired 20-input Source / 20-input Live Preview run on the same host, debug
binary, 1,240 × 800 window and Xvfb display observed glyph pixels after real X11
key events. It measured the glyph interior, excluding caret columns; typing and
Undo first proved that the sampled pixels change and return. All 40 Undo cycles
restored the exact source, checked through the external clipboard.

| Mode | Median | p95 | Maximum |
| --- | ---: | ---: | ---: |
| Source | 83.4 ms | 139.7 ms | 146.6 ms |
| Live Preview | 66.6 ms | 97.7 ms | 108.9 ms |

These are input-dispatch-to-observed-Xvfb-framebuffer measurements, including
xdotool and screenshot sampling overhead. They are not compositor presentation
latencies or evidence that Live Preview is faster. The small fixture and sequential
mode order do not establish large-note performance. Scripts and raw logs are
`/tmp/tessera359-ui/native-geometry.py`,
`/tmp/tessera359-ui/native-timing.py`,
`/tmp/tessera359-native-geometry.log` and
`/tmp/tessera359-native-timing.log` in the development environment.

The available environment has Xvfb but no installed IBus/Fcitx or Wayland
compositor. Real IME preedit/update/commit/cancel and candidate-window geometry
must still be exercised in a suitable native session. Existing API-level tests
must not substitute for that evidence. The complete wrapped-row movement/drag matrix also stays pending. A separate
real X11 probe passed Left over combining Cyrillic, an emoji ZWJ sequence and a
flag, Home/End on the same line, and forward/reversed cross-block drags. Each
selection was verified against canonical source through an externally reset
clipboard. The fixture file remained unchanged. Evidence:
`/tmp/tessera359-ui/native-movement.py` and
`/tmp/tessera359-native-movement.log`. These specific cases do not imply that
all cases in the larger managed matrix have passed.
