# Large-editor residuals: bounded design options

Refs [editing lag](https://git.oklabs.uk/BeFeast/okilum/issues/2803) and
[large edit tools](https://git.oklabs.uk/BeFeast/okilum/issues/2804).
Source binding: main `5eef32d0fd0af0adcd947990dbc53fe85b32c301`, gpui-kit
`928c3eb776a3d733d9b771f7dea27a6a79242ced`, app-lock tree-sitter **0.26.13**,
GPUI **0.3.3**. This is a design for review, not implementation authorization,
a timing result, a leak diagnosis or acceptance of either reporter issue.

## Current mechanism and invariants

Format/Repair prepare exact hunks on the worker; grouped Apply and Undo/Redo
produce real wide InputEdits when most lines change. Rope-indexed point conversion
already avoids whole-source flattening. The adapter edits the pending main Tree
on foreground before querying current byte/point coordinates, then debounces a
background incremental parse. A running parser needs its immutable old Tree;
detached retirement can retain shared subtrees until scheduled. Exclusive trees
still traverse edited descendants; necessary COW and traversal are distinct.
Already shipped retirement/queued-snapshot changes do not remove either one.
The retained 64–67 ms figure is aggregate update_highlighter, not isolated Tree
edit time. Existing C work counters are not milliseconds or RSS.

Any replacement must preserve immediate pending query/fold coordinates, source
identity plus exact-byte fallback, language/generation/cancel binding, parser
reuse, injection precedence, styles, UTF-8/bidi geometry, selection/hit-testing,
complete source, one Undo/Redo group, canonical save/conflict/recovery and small
or wrapped behavior. No neutral-style frame, new threshold, synchronous parser
wait or removal of a required immutable snapshot is acceptable.

## Options, cost and risk

| Option | Work moved or removed | Cost and principal correctness risk | Dependency/gate |
|---|---|---|---|
| Keep edited pending Tree; optimize its edit algorithm | Reduce traversal/COW inside the exact existing representation | Isolate exclusive visits, shared make_mut and child-array copying on actual Format/Undo and sparse Repair first; algorithm work is substantial and counts alone do not establish a latency gain | tree-sitter change; no GPUI core need established. tree-sitter is not vendored today: shipping a patch needs a pinned, checksum-verified vendor patch like `scripts/gpui-pre-*-setup.py`. Any upstream submission needs separate approval |
| Pending syntax projection with an ordered edit log | Keep parsing/reuse Tree snapshots immutable and translate pending query/fold coordinates without foreground Tree edit | High representation cost. Translating capture endpoints alone is insufficient: edits change query ancestry, overlapping injections and deleted/inserted regions. Must reproduce current pending semantics and replay grouped edits in revision order, including Undo/Redo | gpui-kit/base representation change; no gpui-pre change proved necessary. No Oleg gate (gpui-kit patch); choose it over the first option only through bugs-main review of a concrete equivalence contract |
| Exact range-query/injection index within the current model | Avoid visiting irrelevant syntax owners during visible style queries | Smaller possible slice, but source-only candidate. Injection layers are start-sorted; ends are not monotone because combined layers overlap. A prefix-max-end index could seek safely, but all overlap/order/pending-coordinate semantics need an old-path oracle. Existing scan is capped at 512 non-combined parse attempts plus combined language layers; no measured menu cause is attributed to it | gpui-kit only; no GPUI core or tree-sitter change needed for an index. No implementation selected |
| Share/chunk immutable glyph geometry and apply lazy suffix translations | Avoid full glyph-vector copies/coordinate replay for edits in huge lines | High geometry cost. Current GPUI output requires materialized layouts; caret/selection/click geometry far right remains observable. Keep strict wide-coordinate tests; Hebrew/controls/emoji/combining insertion must invalidate ASCII-only state immediately and take the full correct path | May require gpui-pre API/data changes: gated Oleg approval before a concrete core patch; upstream submission separately gated |
| Retire final Send-only Parser/Rope owners on the existing executor | Relocate proven final foreground destruction without moving UI entities or history | Bounded ownership slice only after actual final-destructor work is demonstrated. Parser, source Rope and history have distinct owners. Moving another clone need not move the final free; delaying ownership increases residence and needs a backlog/lifetime analysis | A gpui-kit/app-only transfer may suffice; gpui-pre need is not established. No further private core observer/control is authorized |

The first two options are alternatives, not permission to broaden a small patch.
Start with an actual-path work/semantic oracle for one selected boundary. Do not
repeat the completed ownership matrix, native fallback tests or timing sweeps.
A necessary regression must fire real moved work, fail with compiled old
production and preserve source/geometry semantics after task drain.

## Menu boundary already changed; remaining rendering scope

The reported unwrapped hover reshape was addressed by
[8def743de](https://git.oklabs.uk/BeFeast/okilum/commit/8def743deda9523bb04fb9c8f4a4d91f77175bd1),
landed in [PR #2815](https://git.oklabs.uk/BeFeast/okilum/pulls/2815).
Current file/fence menu callbacks change language only on click. Hover/keyboard
highlight changes PopupMenu selected_index and notifies that entity; opening
captures a small Scope, not source text. The unchanged unwrapped SourceShapeCache
reuses matching revision/presentation/font/scale/viewport/runs. Existing
edit2803_unwrapped_popup_frames_reuse_shapes_and_invalidate_on_edit checks cache
hits without new shape/layout calls and invalidation after a real edit.

This does not bypass all editor prepaint: visible style queries, decorations and
other wrapped/long-line work can still run. The existing injection scan bound
above is not a proof that all such work fits a frame. Prior official beta7689
menu navigation median/max was 22.9/42.5 ms on idunn and 39.1/61.1 ms on Ubuntu;
those receipts are reused, not a new current-build or 16 ms result.

## Separate residual contracts and next evidence

- **XML:** streaming Validate/Format worker logic exists, but wide Apply/Undo,
  mixed content, attributes/entities, Unicode/CRLF, injection styles and complete
  source/one-Undo behavior remain part of this owner's functional/performance
  scope. Preserve meaningful text and the same Validate/Format refusal authority;
  do not substitute a whitespace-only or JSON oracle. No new XML defect or native
  acceptance is inferred from the JSON delivery receipt.
- **Other long lines:** keep no-space numeric/base64, multiple large rows,
  wrapped rows, RTL/combining/emoji and offscreen caret/hit-testing separate.
  Exact-match caches do not make changed-line shaping constant-time. First
  attribute materialization, glyph copying, suffix replay and native shaping to
  actual paths before choosing one bounded change; no threshold relaxation.
- **Necessary sharing:** running parses and detached retirement may share nodes
  naturally. Their work/residence is not a leak. Preserve cancellation ordering
  and immutable reuse; no forced worker drain or discarded parser reuse.
- **Parser/Rope:** identify the last actual owner and executor separately from
  already moved Query/Tree/injection owners. Keep editor reopening, retained
  history and canonical/recovery data lifetimes intact. The consumed private
  GPUI observer/control cannot be repeated; a new gated diagnostic needs a
  concrete separately approved proposal.
- **Wide Tree traversal:** select and review one option above only after the
  pending-coordinate/query equivalence contract is specified. Wide Format/Undo
  spans are real; they cannot be optimized away as accidental whole-file edits.
- **Platform/native:** source/unit controls do not accept installed macOS,
  Windows or Linux behavior. A later separately authorized native plan must bind
  exact beta/source, same scenario/fixture and real input-to-frame evidence per
  platform, including blur/quit/reopen and complete operation results. This
  design performs no build, installation, device run or timing experiment.

All six residual families remain OPEN, unfinished and owned by the original
implementation owner across scoped PRs. They are not reduced to reporter-only
acceptance. Bugs-main owns architectural escalation, independent review,
guarded landing and normal delivery; the implementation owner does not merge,
request new device work or submit upstream changes under this design.
