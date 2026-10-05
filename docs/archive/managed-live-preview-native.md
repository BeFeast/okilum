# Live Preview native adapter — isolated candidate contract

Status: bounded design review, not implemented or accepted natively. Parent: [#211](https://git.oklabs.uk/BeFeast/tessera/issues/211). Root creates the child issue after reviewing this scope. This document permits a synthetic native fixture first; it does not wire BrainView, Save, recovery or backend APIs.

Reviewed source: Tessera `0748c9e32a3f996895223b37e42164d050b42617`, vendor `928c3eb776a3d733d9b771f7dea27a6a79242ced` with the current committed patch set. Foundation #212 is reviewed; classifier #214 remains a separate lane. Overall behavior remains governed by [managed-live-preview.md](/home/example/worktrees/tessera/live-preview-contract/docs/managed-live-preview.md).

## Ownership and candidate boundary

One `EditorState = InputBaseState<EditorMode>` retains its canonical authored `Rope`, source-coordinate selection (including direction/anchor), IME marked range, focus handle, scroll surface and UndoManager. A projection is immutable derived presentation, never another editor buffer or edit authority. A bounded derived display string/Rope for shaping is allowed; it cannot own selection, transactions, recovery or writes.

Application-owned classification supplies generation-bound spans/styles through a small generic vendor seam. Do not introduce a vendor dependency on tessera-core, duplicate the Markdown classifier in the vendor, or patch gpui core. Preserve a small documented, reproducibly applied vendor diff. Existing Source mode and other Input modes retain their behavior when the seam is disabled.

The outer editor API always returns/accepts canonical source coordinates and bytes. This includes `value`, selected text, selection setters, programmatic replacement and change notifications. No transformed Reader/source_preview input enters the seam. Synthetic fixture data is owned throwaway data; no user note writes or BrainView integration.

## Required coordinate and snapshot contract

Use distinct source UTF-8, projected UTF-8, wrapped display position and native source UTF-16 coordinate types or equally explicit conversion boundaries. Existing `BufferPoint.col` and line offsets must not silently change meaning. Projection preserves authored newlines, but byte offsets within every affected line still differ.

The mapping chain is source UTF-8 → immutable projection → styled wrapping/shaping → optional supported display transforms → geometry. Its inverse maps through the same snapshot in reverse. The facade remains source-coordinate based; WrapMap consumes projected text. Every consumer of line offsets must be classified as source or projected, including caret, selections, line starts, cursor scroll, mouse drag, preferred x, Home/End, candidate bounds and character-index queries.

`LastLayout` must pin the exact projection and style/wrap/layout inputs that produced its shaped lines. Pin document identity/source generation **and** presentation/layout epoch: active reveal, mode, styles, font metrics, width, scale and wrapping indent can change geometry without editing source. Never interpret old shaped indices with a newer projection. An old layout can supply a hit only while its source identity/generation remains valid; otherwise defer that geometry action until coherent layout exists. Never substitute zero, document end, or a guessed nearest source range on stale mapping.

Resolve click/hit once against the layout actually displayed → canonical source caret plus explicit conceal bias and wrap-row affinity → update active range → reveal → reflow. Do not hit-test that same pixel again after reveal. Drag keeps its canonical anchor/direction while each subsequent event uses its own coherent layout. Retain a canonical caret/viewport anchor through presentation changes so reflow does not silently reposition the editing target.

Conceal bias and wrap affinity are independent. Specify policies for glyph-side hit, collapsed caret, selection endpoints and directional movement; do not use one global bias for every operation. Adjacent hidden spans share the #212 anchor semantics. Reversed selection and collapsed carets must stay reversed/collapsed as appropriate.

## Mutation, reveal and IME

Reveal belongs inside native selection/composition transitions, before the next layout or destructive edit. External `InputEvent::Change` is insufficient: selection changes need not emit it, IME preedit mutates Rope without emitting it, and undo/redo directly restore selection after silent edits. Include public selection setters, mouse/keyboard movement, select-all, drag, word/line actions, composition start/update/commit/cancel/unmark, undo and redo.

Before an edit, resolve its exact canonical replacement span once and reveal every touched block (including shared boundary blocks); retain the span through reflow. Copy/cut/paste/delete use canonical bytes. Backspace/forward delete must derive grapheme-safe source targets after resolving movement semantics, not subtract a byte in projected text. Check actual explicit IME replacement ranges, which may differ from current selection.

Every accepted Rope mutation immediately invalidates projection/layout authority and advances source generation, including preedit, silent replacement, programmatic replacement, undo/redo and document load. A rejected mutation leaves no projection for speculative bytes usable. Selection/composition-only changes advance presentation state. Stale asynchronous classifier output cannot replace newer source or newer active reveal; an accepted classification is recomposed with current native active state.

Keep native UTF-16 APIs source-based: text/range queries and replacement arguments never become display UTF-16. Candidate geometry converts source UTF-16 → source UTF-8 → pinned projection/layout; point queries invert that chain and return source UTF-16. Preserve existing composition transaction semantics, including commit with no subsequent unmark, consecutive compositions, cancel and selected replacement.

**Grapheme/IME distinction:** current native clipping only guarantees UTF-8 scalar and CRLF boundaries; #212 requires extended grapheme boundaries. The adapter must add grapheme-aware user movement/deletion for the enabled EditorState candidate; no global change to unrelated Input modes is required. A valid native composition range can temporarily lie inside a grapheme (combining marks/ZWJ preedit). Do not round or reject such a valid IME edit merely to fit #212. Use exact editable Source presentation while that active range cannot be safely projected, preserve the exact native replacement span, and resume projection only when current active state is representable. Invalid UTF-16/surrogate boundaries must follow an explicit validated bridge policy, never a display-offset guess.

Projection/reveal/mode/theme/layout updates never call `set_value`, mutate Rope, emit a source Change, advance source generation, clear redo, or invoke UndoManager transaction/coalescing methods. The user's existing movement and edit actions retain their own normal transaction boundaries; reveal adds none. Existing explicit document-load/restore reset APIs are outside this synthetic scope. Ordinary Source/Live Preview toggles retain one undo/redo chain.

## One metric source for wrapping, painting and hit testing

Current TextWrapper wraps plain fragments using one font/size; InputElement later shapes styled TextRuns. Concealment alone does not solve that mismatch. The candidate must use the same visible bytes, font family/weight/style, size, tab/indent handling and wrap width for line breaking and shaped painting/hit geometry. Bold headings and inline monospace code are required positive controls because they expose width differences.

The bounded first fixture may keep a single font size and line height while styling heading weight and inline code family; record that choice explicitly. It must still measure those actual font runs during wrapping. If varying heading sizes/line heights are implemented, row-height/scroll/selection/candidate geometry must share the same measured height model. Never paint larger headings on a uniform-height map and claim acceptance. Unsupported metric/style requests fall back to exact Source, not approximate styled geometry.

Initially leave folding, search/replace overlays, diagnostics, completions, inline hints, masking and unrelated editor adornments disabled in the synthetic fixture unless their full source/display mapping is implemented and tested. A later attempt to combine an unsupported feature with projection must explicitly select Source. This is a candidate boundary, not a claim that those existing Source features are broken.

## Native evidence required before any managed integration

Use one pinned synthetic fixture with supported paragraphs/headings/emphasis/strike/code/links/wikilinks plus unsupported frontmatter/fences/tables/embeds and malformed syntax. Include BOM, CRLF and LF, absent final newline, multibyte scripts, combining marks, emoji ZWJ/flags, long wrapped lines and adjacent hidden markers. Freeze source/binary/vendor/fixture/font/viewport hashes and record real native input with a working positive control.

| Case | Required observation |
| --- | --- |
| Click, arrows, Home/End | Correct canonical caret before/inside/after hidden spans; first/last wrapped glyphs and far-right row hit; wrap affinity survives reveal; user movement never splits graphemes/CRLF. |
| Shift and drag | Forward/reversed selections cross blocks and wraps; canonical anchor survives reflow/scroll; every touched block reveals; released selection and copied bytes agree. |
| Source copy/cut/paste/delete | Raw clipboard/source bytes match canonical spans including markers/terminators; multiline paste, adjacent-boundary deletion and unsupported Source editing have no guessed spans. |
| IME | Real preedit/update/commit/cancel at concealed/wrapped boundaries with CJK and non-BMP/combining content; source UTF-16 ranges remain exact; candidate rectangle follows the displayed caret; subgrapheme Source fallback does not corrupt composition. Pure method calls are supporting tests, not real IME proof. |
| Undo/redo | Typing, paste, cut, delete and composition return exact source/selection; reveal/mode/theme/resize create zero extra transactions and retain redo; normal edit grouping matches Source control. |
| Styled geometry | Bold heading and monospace code close to a wrap threshold have matching wraps, painted glyphs, click targets, selection/caret rectangles and IME bounds at narrow/wide widths. |
| Stale/fallback | Inject stale classification/layout and resource exhaustion, change selection while classification is pending, edit between paint and input; preserve current source and refuse stale geometry. Unsupported text remains editable Source. |
| Responsiveness | Same-host native input/paint measurements on the identified bounded fixture; no typing handler waits for semantic parsing. Root reviews measured outcome separately from correctness. |

Native fixture PASS is a prerequisite for a later managed integration issue, not completion of #211. Until that later gate, no claims about BrainView Save/CAS, true-base handling, recovery, close/navigation or crash/reopen through this adapter.

## Source observations behind this review

- [State ownership and source selection](/home/example/worktrees/tessera/source-projection211/vendor/gpui-component/crates/base/src/input/base/state.rs:291); [scalar-only Rope clipping](/home/example/worktrees/tessera/source-projection211/vendor/gpui-component/crates/base/src/input/base/rope_ext.rs:424); [CRLF cursor clipping](/home/example/worktrees/tessera/source-projection211/vendor/gpui-component/crates/base/src/input/base/state.rs:2351).
- [Silent undo/redo and selection restoration](/home/example/worktrees/tessera/source-projection211/vendor/gpui-component/crates/base/src/input/base/state.rs:2057); [IME preedit path without Change emission](/home/example/worktrees/tessera/source-projection211/vendor/gpui-component/crates/base/src/input/base/state.rs:2889).
- [Plain wrapping](/home/example/worktrees/tessera/source-projection211/vendor/gpui-component/crates/base/src/input/editor/display_map/text_wrapper.rs:292); [styled shaping](/home/example/worktrees/tessera/source-projection211/vendor/gpui-component/crates/base/src/input/base/element.rs:1294); [LastLayout metadata](/home/example/worktrees/tessera/source-projection211/vendor/gpui-component/crates/base/src/input/base/layout.rs:14).

Review verdict: the proposed single-buffer seam is a viable **candidate scope**, conditional on these explicit coordinate, epoch, grapheme/IME and metric requirements. No implementation or native outcome was verified in this pass.

## Isolated implementation checkpoint (#216)

The runnable fixture is `cargo run -p tessera-shell --example native_projection216`.
Its synthetic source and hash live beside the example. This is a development
candidate, not managed integration or native acceptance. The adapter uses the
reviewed #214 classifier asynchronously and wraps the #212 mapping primitive;
vendor code has no dependency on either Tessera module.

The public seam is `input::projection::{ProjectionProvider, SourceProjection}`.
`EditorState::source_stamp()` synchronously reports document identity and source
generation. `SourceMutation` is a separate event stream covering accepted edits,
preedit, silent replacement/undo and resets. The existing `InputEvent::Change`
contract remains distinct. `set_projection_provider` changes presentation only;
`None` selects Source while retaining the opted-in candidate's grapheme navigation
and validated source UTF-16 bridge. Invalid surrogate-half replacement positions
are rejected before transaction changes; valid subgrapheme composition remains
exact Source.

`LastLayout` pins the projection, source stamp, presentation epoch, font, size,
scale, width and line height. WrapMap consumes projected bytes behind a facade
that returns source coordinates. Wrapping and painting consume the same font-run
builder. This fixture uses one font size/line height, bold headings and an inline
code font; disabling soft wrap, continuation indentation, folding, search,
diagnostics, decorations and language-service providers require Source fallback.

Nearest glyph-boundary hits use right conceal bias; hits above a displayed row use
left bias. Wrap affinity remains independent. CRLF's zero-width carriage return
maps to the end of its preceding visual row; user hits and wrap breaks never split
a grapheme. A source-stale mouse-down waits for a coherent paint; that paint
resolves the click once before reveal/reflow. A released deferred click does not
restart dragging. Dragging below a partial viewport reaches its displayed endpoint
and lets auto-scroll expose subsequent rows.

Supporting checks cover the application adapter, native input engine, strict
UTF-16 handling, grapheme/CRLF deletion and redo through presentation changes.
These do not replace real keyboard/mouse/clipboard/IME evidence. Native acceptance,
styled threshold measurements and full independent source review remain pending.
The existing GPUI Wayland IME path's treatment of preedit cursor positions is an
explicit item for the real Source/Live Preview comparison; gpui core is unmodified.

### Source review corrections

The additive `0011-input-projection-review.diff` keeps the original candidate
patch intact. Projected paint runs include logical LF separators, including empty
lines; mapped native composition underlines overlay those runs without changing
font metrics. User word/line selection expands to grapheme/CRLF boundaries.
Public byte-range selection retains exact scalar endpoints and synchronously
restores an incompatible old projection map before another input event.

Offscreen projected vertical navigation measures the pinned font runs and wrapped
rows. Offscreen Source fallback can contain syntax/decorations with other fonts,
so it scrolls to the canonical target without moving the selection, then resolves
once against the painted target; intervening source or selection changes cancel
the pending move. The candidate explicitly falls back to Source when soft wrap is
disabled, leaving horizontal extent in the existing Source implementation.

### Native IME geometry refresh

The additive `0012-input-ime-geometry-refresh.diff` requests the public GPUI
`Window::invalidate_character_coordinates()` API after a focused opted-in editor
installs coherent source/presentation geometry. It uses GPUI's existing native
candidate-bounds policy and caches the actual rectangle, so identical idle paints
do not schedule repeated updates. The deferred platform query reads current source
UTF-16 state; no captured projected offsets or extra editor mutations are used.
This addresses a first-preedit rectangle delay observed in the native Source
baseline. Real wire validation remains separate from the supporting tests.

The example's optional `TESSERA_NATIVE216_TIMING=1` instrumentation records source
mutation observations and a trailing CPU paint marker with source generation and
presentation epoch. It labels CPU paint separately from compositor presentation;
normal example runs leave this instrumentation disabled.

### Exact external clipboard candidate (#218)

The additive `0013-input-exact-clipboard.diff` exposes
`input::clipboard::ExactClipboardProvider` and typed `ClipboardPasteEvent` results.
Installing a provider opts the editor into exact paste immediately, even before
classification; removing it retains that opt-in and reports `Unsupported`.
The application owns the Linux Wayland implementation in
`crates/tessera-shell/src/platform/exact_wayland_clipboard.rs`, loaded only by the
isolated example. GPUI core remains unmodified. The ordinary GPUI Wayland reader
normalizes CRLF, so this opted-in path never falls back to its already-normalized
text after an unavailable or failed exact read.

The provider reads the regular selection via `zwlr_data_control_manager_v1` on the
same explicit named socket as the fixture. Before GPUI initialization the fixture
resolves its named `WAYLAND_DISPLAY` and removes inherited `WAYLAND_SOCKET`; if a
named socket cannot be verified, exact paste is unsupported. Multiple seats require
an unambiguous `TESSERA_NATIVE216_SEAT`. No clipboard ownership, external executable,
compositor configuration or managed editor integration is involved.

Each request has a two-second total deadline, an incremental 8 MiB limit and strict
UTF-8 decoding. BOM, CRLF, LF, lone CR, Unicode and whitespace remain exact. Empty
text is a valid replacement; absent selection is `Unavailable`. The read pins the
selected seat and offer, polls transport and pipe concurrently, and completes a
same-connection synchronization barrier after EOF. Selection replacement/removal,
selected-seat removal and device termination reject the pending read. An unrelated
offer announcement does not change the selection. Every failure releases its file
descriptors and connection.

One physical read is allowed per editor. A second paste reports `Busy` without
cancelling the first or advancing its request identity. Source, selection/direction,
wrap affinity, composition, focus, window activation, editability or provider
changes invalidate the pending request, including change-and-return transitions.
Cancellation retains the busy slot until physical completion; the provider checks
cancellation at most every 20 ms and shares the finite deadline across all phases.
Completion applies only to the original entity/window and exact state revision.
Failures and stale results do not mutate source or history; accepted text uses one
existing atomic replacement transaction.

Supporting tests exercise asynchronous editor delivery, busy-slot preservation,
state invalidation, empty/unavailable/error results and real pipe bytes/deadlines.
The fixture-only `TESSERA_NATIVE216_CLIPBOARD_DELAY_MS` (default zero, maximum 1500)
delays delivery within the same total deadline for deterministic native cancellation
and repeated-paste evidence. Native clipboard acceptance remains a separate gate.

### Vertical movement through reveal (#219)

The additive `0014-input-target-active-vertical.diff` resolves projected vertical
movement synchronously before publishing the final canonical caret. It composes
current active presentation, measures the target, then composes and measures the
hypothetical target-active presentation with the same pinned font, width and wrap
policy. Entering a revealed logical line from below uses its newly wrapped final
row; entering from above uses its first row. The final active projection must agree
on text, styles and the selected source mapping before the move is accepted.

The initial preferred x is measured from current canonical caret geometry when no
prior movement established it, then retained across later vertical actions and
short-row clamps. Each rapid key derives geometry from the current canonical
active state. No provisional selection is exposed between input events, so typing,
delete, paste and native edits cannot race a later caret correction. The displayed
layout remains authoritative for mouse hits; the click-once-before-reveal policy
is unchanged. Unsupported or stale derived geometry leaves selection untouched.

Supporting tests cover actual initial paint followed by the first arrow, repeated
arrows without intermediate paint, EOF clamping, entering a target whose reveal
adds a wrap row, and immediate type/delete/paste at the final caret. Native geometry
and responsiveness remain separate acceptance checks.
