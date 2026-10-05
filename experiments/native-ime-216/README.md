# Native Wayland test input method for #216

This isolated experiment sends real `input-method-v2` requests through Hyprland to
the application's `text-input-v3` backend. It creates an actual `wl_shm`
`input_popup_surface_v2` and logs compositor-delivered candidate rectangles.
It does not call GPUI methods or turn wtype Unicode into supposed IME evidence.

The helper is a deterministic test input method, not a production CJK engine.
It has no keyboard grab, dictionary, reconversion, surrounding-text deletion, or
interior preedit cursor controls. Its fixed popup labels `IME 216`, `1 NI`, and
`2 HON` identify synthetic candidates; changing the highlighted row does not
commit or convert text. Existing editor selections can still be replaced by
preedit/commit through the real wire path.

## Build

Use the existing C compiler, wayland-scanner, and libwayland-client development
files. No package, service, or desktop configuration changes are needed.

```sh
bash experiments/native-ime-216/build.sh
experiments/native-ime-216/build/ime216 --help
```

The build uses `/usr/bin/cc`, avoiding a PATH wrapper. Generated protocol bindings
and the binary stay under ignored `build/`. The copied protocol XML is from
`wayland-protocols-misc 0.3.12`, SHA256
`33ac1a0325dcfefd566773928917b0251f54078d0f530852cbfe82aa0270a0d9`.
Its protocol and copyright notice remain intact.

## Controlled native execution

The native rig owner must coordinate runtime. Do not launch GUI or start the rig
as part of the build. The helper requires an explicit absolute runtime directory,
rig socket basename, and timeout from 1 to 1800 seconds. There is no ambient socket
default. It clears `WAYLAND_SOCKET` in its own process before connecting because
libwayland otherwise prefers that inherited file descriptor even when given an
explicit socket name. Verify the rig socket/signature differ from the desktop before starting.
Keep the helper in tracked process orchestration with stdin open; do not use a
detached `rig/run.sh` launch, unqualified hyprctl, or ydotool.

The owner can invoke the built binary as follows after establishing those inputs:

```sh
./build/ime216 "$XDG_RUNTIME_DIR" "$RIG_WAYLAND_DISPLAY" 600
```

JSON lines are written to stdout; optional `WAYLAND_DEBUG=client` goes to stderr.
The owner should retain both streams with the frozen app/helper/fixture/font/XML
hashes, viewport/scale, exact rig socket, and compositor version. Protocol capture
must contain only the owned synthetic test processes.

The helper refuses missing/ambiguous seat or input-method globals, absence of
text-input-v3, another active IME (`unavailable`), and composition while inactive.
Wait for `activate_pending` followed by `done` with `active: true` before sending
composition commands. Each command occupies one UTF-8 line on stdin:

```text
status
preedit に
preedit 日本
row 2
commit 日本
preedit é
cancel
quit
```

Send one step at a time and wait for independent application evidence between
steps. The command roundtrip acknowledges processing by the compositor, **not**
GPUI application or painting. Do not use elapsed time or request logs alone as
proof of a transition. F8 captures the fixture without clicking a button and
changing focus. Normal navigation/F7/F8 may be sent by the existing rig keyboard
helper because this test input method does not grab the keyboard.

- `preedit TEXT` sends the exact UTF-8 text with its cursor at the byte end.
- `commit TEXT` clears preedit and sends a committed string in the same state batch.
- `cancel` clears preedit without a committed string. Do not presume this restores
  an original selection that preedit already replaced; compare Source behavior.
- `row 1` / `row 2` change only the synthetic candidate highlight.
- `status` logs active state and the current input-method serial.
- `quit` / stdin EOF clean up the owned protocol objects. Cancel explicitly before
  exiting if a preedit is active; disconnect is not claimed to restore source.

Input is bounded to 4095 bytes per command line and 3900 bytes per text payload.
Invalid UTF-8, embedded NUL, unknown commands, inactive composition, display errors,
and buffer backpressure fail the run. Each commit uses the number of received
input-method `done` events as its serial. State is applied at `done`; callbacks
never automatically echo composition requests. Overall timeout exits 124 and
closes the process resources; it is not a passing receipt.

## Required proof and limits

Run the same Source and Live Preview cases: distinct preedit/update, commit,
cancel, consecutive compositions, selected replacement, CJK, non-BMP/combining
payloads, hidden-marker and wrap boundaries, and undo/redo. Require intermediate
screenshots, exact F8 source/selection snapshots, separate SourceMutation/Change
logs, and incoming application protocol events. A no-change assertion needs a
working composition positive control from the same session.

The 260×108 popup has a yellow border and two identifiable rows. Capture it near
at least two distinct caret positions and after wrap/resize. Correlate GPUI
`set_cursor_rectangle` with received `popup_rectangle` events and screenshots.
Application rectangles use application-surface coordinates; received rectangles
use popup-surface coordinates. Their numbers need not be equal. Account for
window placement, output scale, and compositor edge avoidance when checking the
visible candidate panel against the displayed caret.

The inspected GPUI 0.3.3 backend ignores incoming preedit cursor offsets, has no
`DeleteSurroundingText` handler, and does not send surrounding text. This helper
cannot turn those gaps into coverage. Direct API explicit replacement and
subgrapheme tests remain separate supporting evidence. Read-only source findings
are not themselves a native runtime failure verdict. Do not claim overall #216
PASS, production-engine acceptance, or managed integration from helper success.
