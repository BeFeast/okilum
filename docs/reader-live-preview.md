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

Publication as a PR waits for the executor CI slot and #699's save fix. Merge
waits for the remaining native acceptance rows, using the PR Linux package on
muninn as described below. This is not a released-build claim.

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
`~/.cache/tessera-qa/359/tessera359-ui/native-geometry.py`,
`~/.cache/tessera-qa/359/tessera359-ui/native-timing.py`,
`~/.cache/tessera-qa/359/tessera359-native-geometry.log` and
`~/.cache/tessera-qa/359/tessera359-native-timing.log` in the development environment.

The available environment has Xvfb but no installed IBus/Fcitx or Wayland
compositor. Real IME preedit/update/commit/cancel and candidate-window geometry
must still be exercised in a suitable native session. Existing API-level tests
must not substitute for that evidence. The complete wrapped-row movement/drag matrix also stays pending. A separate
real X11 probe passed Left over combining Cyrillic, an emoji ZWJ sequence and a
flag, Home/End on the same line, and forward/reversed cross-block drags. Each
selection was verified against canonical source through an externally reset
clipboard. The fixture file remained unchanged. Evidence:
`~/.cache/tessera-qa/359/tessera359-ui/native-movement.py` and
`~/.cache/tessera-qa/359/tessera359-native-movement.log`. These specific cases do not imply that
all cases in the larger managed matrix have passed.

## Muninn pre-merge acceptance plan

Owner-approved QA environment: Omarchy, Hyprland/Wayland on muninn, fcitx5 with
an installed Chinese or Japanese composition engine. QA runs the Linux package
from this PR's `linux-release` CI artifact before merge. First merge #699 / PR
#702; then publish S1 into the free executor CI slot. When the package is ready,
report the exact run number, artifact and source commit to the manager so the
QA sub-session can start. Do not substitute the public beta or an older binary.

The Linux workflow filters automatic PR runs to packaging/workflow changes.
For S1, dispatch `linux-release.yml` explicitly on the published feature branch
after its required checks finish. Its `arch-qa` artifact supplies the QA package;
branch dispatch never enters the public beta publisher. Record that dispatch
run number separately from the ordinary PR CI run. This uses the existing
workflow and does not require a packaging-only change to trigger CI.

Use a disposable vault copy. Record package version/commit, compositor, input
engine, font, scale and window width. Repeat in Source and Live Preview on the
same machine/session, at wide and narrow widths. The primary acceptance priority
is lossless ru/he/en input and layout switching; CJK composition additionally
exercises native preedit and candidate geometry.

| Scenario | Required result |
| --- | --- |
| Switch en → ru → he → en while editing | Every committed character appears exactly once; switching does not drop, duplicate or replace existing text. Repeat after moving the caret and with a selection. |
| Cyrillic and Hebrew mixed with Latin, digits and punctuation | Source, external clipboard and saved/reopened UTF-8 bytes match the authored sequence. Compare mixed RTL/LTR caret and selection behavior in both modes; do not infer bytes solely from visual order. |
| Combining marks, emoji ZWJ and flags beside styled links | Movement/deletion preserves whole graphemes; copy retains authored Markdown and Unicode. Include Cyrillic combining marks and Hebrew niqqud. |
| CJK preedit/update/commit/cancel | Preedit reveals the affected source block; candidate window follows the actual caret. Commit inserts exactly once; cancel leaves authored bytes unchanged. Repeat selected replacement, successive compositions and Undo/redo. |
| Wrap boundaries | At both widths, click first/last glyph and far-right row space; arrows and Home/End resolve to canonical source positions. Forward/reverse drag across wraps keeps its anchor through reveal. Include bold, inline code, links and mixed ru/he/en text. |
| IME at wrapped/concealed boundaries | Candidate geometry follows the caret after reveal, resize and scroll; no jump to document start or stale row. Preedit, commit and cancel retain exact source coordinates. |
| Presentation toggles and persistence | Toggle without saving or adding an Undo transaction; Undo/redo survives reveal. Explicit Save, external conflict and crash recovery retain authored bytes and newer drafts. |

For each row report PASS/FAIL with the source fixture, expected/actual bytes and
focused screenshot/video where geometry matters. A successfully committed input
and visible candidate list are positive controls for IME; an absent popup alone
is not evidence. A failure in either mode must identify whether it predates S1.
All required rows must pass before merge; API-level tests alone do not close them.
Linux diagnostics: `~/.local/state/tessera/reader-diagnostic.log` (or the same
path under `XDG_STATE_HOME`). Store captures/packages in
`~/.cache/tessera-qa/359/`, never `/tmp`; remove scratch after merge.

### Integration with directory-bound saves (#699)

S1 was rebased locally onto PR #702 commit `b2d1fcf` (main base `98fdecb`).
All 14 Reader editor tests, shell clippy --tests, fmt and vendor verification
passed on that combined tree. The actual Linux/X11 lifecycle probe was rerun
against the integrated binary: exact BOM/CRLF/Unicode clipboard, authored edits,
Undo/redo across presentation toggles, explicit Save, external-conflict refusal,
durable journal observed before SIGKILL, restart into Reader and exact draft
restoration all passed. The external canonical hash and recovered-draft hash
match the earlier evidence above. Logs and the probe live under
`~/.cache/tessera-qa/359/integrated-*`. This closes the local integration check;
it does not replace muninn's pending Wayland/IME acceptance or imply release.

## S2: themed link and heading styles

S2 is a local follow-up to S1 (#714), not a new editing mode. A narrow gpui-kit
patch adds an optional foreground to `ProjectionStyle`; absent colors inherit
exactly as before. Wrapping and painting share the same run builder, including
IME underline splits. Font size and line height remain unchanged (S6 owns those).

Live Preview uses the existing Reader palette: the link token for Markdown links
and wikilinks, and the foreground token plus bold weight for headings. A link
inside a heading retains heading weight and uses link color. Nested emphasis,
code fonts and strikethrough remain composable. Plain Source remains unchanged.

Theme colors wrap the accepted immutable classifier result. Changing theme
reinstalls only that presentation wrapper; it does not reparse Markdown, replace
the buffer, save, alter the source revision or add an Undo transaction. Colors are
chosen again when a background classification result is accepted, so a job that
finishes after a theme change cannot reinstall the old palette. The Reader render
path also refreshes the wrapper when an accepted editor's palette changes.

The S1 native acceptance gate still applies. S2 additionally checks both themes,
link color across wrapped rows, mixed heading/link emphasis, and changing theme
without typing. Neither a provider test nor a screenshot replaces muninn IME QA.

### S2 local validation (2026-10-07)

Rust 1.99.0: 17 Reader editor tests and 7 source-presentation tests passed,
including theme transitions, an unchanged presentation epoch on repeated redraw,
nested link/heading styles, exact mappings and the native screenshot fixture.
Strict shell clippy, fmt, build and the complete vendor patch verification passed.
The added vendor-only marked-run test was not executed: the root workspace cannot
run gpui-base's dev-dependency tests as a non-member package.

Linux/X11 native light/dark captures show actual concealed Markdown, bold
headings and link foreground across wrapped rows. The exact clipboard,
BOM/CRLF/Unicode input, Undo/redo across toggles, explicit Save, external-conflict
refusal, durable journal and crash recovery probe passed again. Evidence and
scripts are in `~/.cache/tessera-qa/359-s2/`; before captures are historical S1
captures on the same host, and the after fixture adds a wrapped link. They are
visual references, not a timing comparison or a substitute for Wayland IME QA.

Native inspection also found an S1 integration defect: `searchable(true)` makes
the vendor prepaint discard even an accepted projection. The separate local fix
`bd37400` disables native source search while Live Preview is active. Invoking
Find temporarily returns to Source and opens its search; closing Find with Escape
or its close button restores the presentation from before Find (#806). Reopening
Find while it is open does not overwrite that choice. An explicit switch to Live
Preview closes search and supersedes the temporary return. The added GPUI regression verifies both transitions without changing
source revision or bytes. Classifier acceptance alone did not detect this bug;
the final native captures provide the rendered positive control. S1 must include
this fix before its final acceptance. No gpui-core patch is involved.

## Roadmap and stability follow-up

The S3–S7 roadmap remains in [research/359-smart-editor.md §9](research/359-smart-editor.md#9-recommendation-and-slice-plan). Native Linux reproduction, acceptance criteria and the owner-reference comparison are in [research/754-live-preview-stability.md](research/754-live-preview-stability.md). The proposed stability work is prioritized before #753; it is not an implementation or acceptance claim.
