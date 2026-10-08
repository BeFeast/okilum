## Native idle investigation (2026-10-08)

P1 #755 remains open; continuous Mac beta8326 jumping is **not reproduced** in the two Linux scenarios below. This does not dispute the owner report or close the issue. No speculative product fix was applied.

Instrumented full Reader, not just isolated widget: main a075d27 (+ documentation-only3945e64), Rust1.99, actual S2 projection, Linux/Xvfb1240×850 at scale1, light theme. Source fixture28,217 bytes,30 sections, mixed Latin/Hebrew/Cyrillic, links with long concealed URLs, bold and inline code. Native source/editor caret was focused. Verified vendor stack before instrumentation and after restoring it; gpui core untouched. Temporary diagnostic edits and reproduction scripts are preserved in the evidence bundle, not shipped. Timing instrumentation can affect scheduling; repeat on release/Mac before closure.

| Counter / observation | At document top (60.091s) | Scrolled, active long paragraph (60.137s) |
| --- | ---: | ---: |
| X framebuffer samples |304|300|
| Provider adoption |0|0|
| Classifier job starts |0|0|
| ActiveSource changes (reveal) |0|0|
| Projection-map installations |0|0|
| Layout metric changes |0|0|
| Raw filesystem events / applied batches |0/0|0/0|
| `layout_lines` calls |120|120|
| Source generation / presentation epoch changes |0/0|0/0|
| Width / scroll |658px /0, constant|658px /−138px, constant|
| Text displacement |0px|0px|

Pixel comparisons cover the central content column, excluding sidebar relative-time labels. Every changed pixel belongs to the2px caret: first run x301..302/y105..123; second run's exact bounds are in pixels.json. Marker/link/paragraph pixels outside that caret rectangle are unchanged in every sample. Visible caret alternation is a positive control for active repaint, not a static/dead window. Samples run around5Hz with monotonic timestamps; videos preserve measured sample intervals, not monitor presentation timing.

**Do not call this “zero relayout”:** literal `layout_lines` invocation count is120/minute, caused by the existing caret repaint path. The requested zero-call criterion is not met. However projection/wrap installation, changed metrics, changed epoch, changed width and text shifts are all zero. Optimizing unchanged repaint layout is separate from proving the reported jumping cause; no cache shortcut is proposed without invalidation/mapping tests.

Positive controls outside the60-second interval: typing one character then Undo produced2 classifier starts and2 provider adoptions in each run. Creating Control.md, then20 writes each to `.obsidian/workspace.json` and `.syncthing.noise.tmp` delivered167/164 raw watcher events and1 applied batch. No additional classifier/provider changes occurred beyond the input+Undo control. Creating the regular Markdown note proves the watcher delivery counter and batch path work; excluded service files do not reload the edited buffer in this Linux setup.

### Hypotheses

- (a) Blink affects paint visibility, but ActiveSource uses selection/composition/affinity, not blink visibility. Both runtime runs show unchanged reveal/projection epoch across120 layout calls. A universal blink→reveal loop is not supported by these results.
- (b) Reader schedules classification on SourceMutation and explicit enable, with accepted source-stamp checks; theme refresh compares accepted colours. No idle jobs or repeated provider adoption observed. Actual jobs were detected in the input control.
- (c) No files changed during baseline. Synthetic service-file writes did arrive at the watcher but did not reclassify/reinstall the editor. Mac FSEvents/Syncthing-specific behavior is not excluded; exact log and event path still needed.
- (d) Tested fixed focus, pointer parked outside text, visible caret, a scrolled active paragraph and collapsed header. Width658px and scroll offset stay fixed in both runs. This does not exhaust macOS overlay scrollbars, fractional/Retina scale, system theme transitions or window resizing.

### Next evidence needed

Manager has been asked for the Mac diagnostic log plus a minimal sanitized Markdown fixture and caret/scroll position, font/scale/theme and Find state. Request goes through manager only. Reproduce under the Mac configuration before selecting a fix; #755 is not resolved by the interaction fixes proposed in #754. Priority remains before #753. The exact source/hash and full fixture are in the attached bundle.

## Mac evidence and portable diagnostic follow-up

The owner's 300-record beta8326 tail ends at 06:44:20 UTC (09:44:20 Israel), before the reported idle interval. It contains four `save_source_queued` events; each is followed by three `incremental_update`/`incremental_ui_publish` pairs within six seconds. This supports investigating save echo but does not instrument the idle render path.

`TESSERA_EDITOR_LAYOUT_DIAGNOSTICS=1` enables per-editor counters and one `editor_layout_sample` per second in the normal diagnostic log. It works in debug and release on Linux/macOS without the Linux-only attribution clock. Start a new process with the variable set, enter Source/Live Preview, then leave it untouched for60 seconds. Counter values are cumulative: subtract consecutive samples for the same document ID. New editors have fresh counters. The timer exits when that editor is replaced/closed; sampling neither calls notify nor draws.

Fields distinguish `layout_calls` (including caret repaint), `metric_changes`, `provider_applies`, `projection_composes`, and `active_changes`. Source generation, presentation epoch, content bounds, caret bounds and scroll offsets make changes correlatable. Caret bounds are in editor content coordinates; account for scroll before interpreting screen displacement. No source text, clipboard contents or selections are logged. Existing diagnostic rotation remains in force.

Use `TESSERA_EDITOR_LAYOUT_DIAGNOSTICS=1 /Applications/Tessera.app/Contents/MacOS/tessera` on Mac after closing the ordinary instance. The log is `~/Library/Application Support/uk.oklabs.tessera/reader-diagnostic.log`; on Linux it is `~/.local/state/tessera/reader-diagnostic.log` (or the corresponding XDG_STATE_HOME location). This is process-local; no launchd/system setting changes.

`editor_disk_refresh` reports `unchanged`, `replaced`, `conflict` or `error`, with generation and presentation epoch. The existing refresh compares exact bytes before `set_value` (stronger than content-hash equality); same-content reconciliation is not deliberately skipped for the rest of the vault/index. A regression sends three same-byte metadata/save echoes through actual incremental publication, asserting editor identity, source stamp, projection epoch, provider identity, selection, scroll and source stay unchanged. A different-byte external update is its positive control.
