# Board + Manager reference (#2464 / #2465)

The opt-in `project-ui-harness` mode `performance` mounts the production Reader,
Manager, composer and Board. Its synthetic data contains 200 completed runs,
1,200 timeline items (Markdown, source-keyed tool-call groups and response folds),
and 240 cards divided equally between Queue, In progress, Checks and Can try.
After #2555 these retain fixed-width independent lanes, compact cards, the
eight-row Queue preview and the Verified rail. At the reference center width,
additional lanes are reached through main's horizontal scroll. Do not restore
the former fluid card layout just to reproduce an older screenshot.
Production defaults to the Board v2 List view. This reference explicitly selects
Kanban so the original 240-card scroll/resize workload remains comparable. Both
views retain main's origin/PR/recency ordering; sorting runs when their source
presentation is rebuilt and is reused during viewport work. Synthetic source
update timestamps keep the fixture's original ascending card order.
It uses a fake Manager gateway and a Board-owned synthetic issue reader. No real
threads, provider dispatches, credentials or installed application are involved.

Use the prescribed `H=github scripts/agent/gh-check.sh` for ordinary acceptance.
Build native artifacts only in an approved hardware build slot; never run Cargo
on maestro except fmt. Prepare **and verify** the unchanged vendor patch stack.
The `manager-board-fixture` GitHub workflow also builds the exact committed
release artifact with source/binary hashes (manual dispatch, or relevant
`perf-fixture/**` pushes). It builds an artifact, not an acceptance verdict.
The native reference uses the shipped release optimization profile:

```
cargo build --locked --offline --release -p okilum-shell --bin okilum \
  --features project-ui-harness
```

Run before and after on **one muninn or baldr**, under **one GUI lock for the whole
session**, with isolated state/config/cache directories and a private headless
sway/DBus session. Launch only the copied artifact, setting:

```
OKILUM_PROJECT_UI_FIXTURE=performance OKILUM_STATE_DIR=<isolated-state-directory>
```

The initial asynchronous note publication can show the note. Enter Board through
the real `← Board` action and verify all four 60-card lanes alongside Manager
before measuring. Use explicit `px` units for sway's floating resize command.
The reference capture is 1196 × 772 client pixels (1200 × 800 decorated window),
scale 1, Dark, Manager about 560 px wide. Retain the actual sway window geometry,
profile widths and artifact/source SHA256s. Do not compare debug and release
numbers, different hosts, or captures clipped by the output.
Verify the rendering backend too: on muninn use a headless `gles2` compositor
with `WLR_RENDER_DRM_DEVICE=/dev/dri/renderD128`. Record positive i915 render-engine
activity for the measured process and reject an accidental llvmpipe reference.
If backend configuration changes, repeat **both** artifacts under that backend
while retaining the GUI lock. Software-rendering diagnostics remain separate.

The harness enables GPUI's **existing** profiler, with no gpui core patch.
`PERF` lines contain UTC epoch end time, CPU `Window::draw` milliseconds and
invalidation count. `PRESENT` records GPU/platform submission separately.
`WORK` is a 250 ms diagnostic aggregate of Board renders, card builds and
Manager row builds; it is not an exact per-frame gate. Composer and fixed-header build/request/prepaint/paint counters separate
residual work from the enclosing Reader stages. The Rust regression uses
per-render card counts, including each follow-up measurement frame separately.

Physical input and positive controls:

- Scroll the Manager at 60 events/s in three 2-second bursts, two toward older
  history and one back. A Wayland pixel stream must set `AxisSource::Finger`
  before Axis/Frame; GPUI ignores raw Wheel Axis without discrete steps. The
  reference sends ±18 pixel values, scaled to ±54 px by GPUI's Wayland adapter.
  Retain before/after captures for **each** burst and reject an unchanged view.
- Drag the real splitter through ±100 px in four alternating 2-second bursts,
  120 motion events each. Read the durable Reader panel width on release;
  each burst must change it by at least 80 px, with changed splitter/header
  geometry in the captures (composer caret and hover are excluded). Restore the initial width before
  the following scenario. Retain before/after captures.
- Click the first visible Working card (#2 in the current fixture) 13 times, at least one second apart, with the
  pointer settled before the first activation. Each before/after card crop must
  change, proving an actual disclosure response. Record input send times and
  first subsequent presentation. This measures send-to-submission latency,
  including compositor/input queueing, not physical display scanout.
- Open the actual model control, send native Escape and type a slow focus-proof
  string. The popup must disappear, the composer must receive that text and the
  selected Board card/model must remain unchanged. Never press Send.
  With `wtype`, use `-s 300` before the first key in **each invocation**: the
  virtual keyboard/keymap needs to settle, otherwise the first key can be lost.
  Prove Escape delivery separately by opening Quick Open with Ctrl+K and closing
  it with the same delayed Escape. Typed letters alone do not prove Escape fired.
- Cancel a held splitter drag by moving focus to another private workspace,
  then return and move with the button still held. The width must stay fixed.
  Keep a virtual keyboard connected for this entire control: a pointer-only sway
  seat does not deliver the required keyboard activation boundary. Verify the
  seat has keyboard capability before interpreting the focus-loss result.

Store ranges as `<phase>-ranges.json`, an array of objects with `scenario`,
`rep`, `start`, `end` (epoch seconds). Resize entries also have `width_before`
and `width_after`. Store the app log as `<phase>.log`, and screenshots as
`<phase>-<scenario>-<rep>-before.png` / `...-after.png`. With Python and Pillow:

```
python3 scripts/perf/manager-board/analyze.py QA_DIRECTORY before
python3 scripts/perf/manager-board/analyze.py QA_DIRECTORY after --gate
```

Missing ranges, zero drawn frames, unmoved splitter, unchanged scroll captures,
unchanged card crops or missing click presentation produce **no verdict**.
Acceptance requires scroll and resize CPU draw p95 ≤ 8 ms; report the share of
frames over 16.7 ms and click latency separately. Keep platform submission costs
separate so a headless compositor's pacing cannot be mistaken for CPU layout.
The reference retains natural card heights and actual text reflow throughout a
splitter drag; it does not freeze content until release. Offscreen lane state
retains height/focus hints only, with no offscreen interaction tree. Source
projection and column membership are reused between publications, with a
second-resolution freshness boundary. Splitter geometry invalidates its own
layout signal; actual Reader publications still invalidate the fixed header.
The Reader-mounted regression bounds fixed-header rebuilds during both inputs.
The Linux splitter uses pointer capture instead of a generic DnD preview,
which otherwise forces a whole-window refresh on every motion. Capture is
released on pointer-up, cancellation, panel closure and focus loss.
Repaints transfer only an owned grab and cannot reacquire a released pointer.
Measured lane extents invalidate a rendered geometry leaf, preserving the
canonical projection cache while still updating the real scroll extent.
Height/focus hints are window-local cells inside one retained lane map. They do
not register an entity dependency for every offscreen card on each resize step.
Linux uses bundled mimalloc for Rust and native C allocations, including the
short-lived layout and interaction nodes;
its effect is checked in the actual release artifact on the same reference rig.
The compact pull badge uses the same Base Ghost/focus/keyboard contract as card
disclosures and retains its original source-bound URL and natural spacing.

The ordinary Linux Rust suite includes the Reader-mounted reference regression,
its real wheel/drag/card-tail controls and Reader Escape/focus assertions. It
rejects Board card builds during Manager scrolling, >64 card builds in any
splitter frame, >64 card/lane layout nodes per frame, >96 Manager row builds
per settled scroll/resize step, and an
unreachable last card. The standalone blocked-picker regression covers sending,
offline, inactive and model-loading state without sends/model writes/draft loss.

Main #2555 keeps card actions in a separate bounded details area; the legacy
baseline expands cards in place. Record that UI difference with the measured
source, calibrate click coordinates from each real capture, and keep all
physical-input/source-change controls. The canonical data volume and CPU draw
gate remain unchanged.
