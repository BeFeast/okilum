# Syntax snapshot ownership and background retirement

Residual: [editing lag](https://git.oklabs.uk/BeFeast/okilum/issues/2803) and
[large edit tools](https://git.oklabs.uk/BeFeast/okilum/issues/2804).
Source audit baseline: `fff9afc17139bf024b4a18acdc46e4c50890ffd7`.
Generated gpui-kit source: pinned `928c3eb776a3d733d9b771f7dea27a6a79242ced`,
including patch 0140's clone-identity equality shortcut.

This change moves ownership of replaced/rejected syntax data into a cleanup
batch. It does not defer or omit `Tree::edit`, clear pending syntax, change
parser reuse, add a size threshold, or claim the retained 64–67 ms phase is gone.

## Current lifecycle and owners

| Stage | UI ownership/work | Background ownership/work |
|---|---|---|
| Before edit | The adapter owns `Rc<RefCell<SyntaxHighlighter>>`, its current Rope, main Tree and injection layers. Styles are queried from those trees into temporary owned vectors; there is no persistent separate style-run cache here. | A previous parse may hold its own Tree/Rope/injection-reuse snapshots. Tree handles share reference-counted subtrees. |
| Replacement | Base constructs byte/point positions with Rope indexes. Above the existing 256 KiB synchronous-parse policy the adapter calls `edit_tree`, which mutates the current tree and stores a Rope clone. This work remains on UI. | No delayed structural edit is introduced. |
| Parse admission | UI clones the edited Tree handle and injection reuse metadata, including each layer's ranges/query handle/tree handle. The existing task and cancellation guard own those snapshots until work starts. | After the existing debounce, the parser receives the edited old tree, source Rope and reuse metadata. Parsing, injections and fold extraction run here. |
| Completion | The foreground continuation holds the incoming Tree/injection/fold vectors and the captured source snapshot. The existing source guard admits or rejects it; 0140 avoids a full equality scan for unmodified clones but retains the exact-byte fallback. | Worker-local old-tree/reuse snapshots retire as their existing scopes end. |
| Accepted publication | Both new owners replace the old Tree and injection Vec synchronously. Previously assignment immediately dropped the old owners on UI. The new method returns those exact owners without cloning them. | One cleanup task receives that batch and drops its Tree, layer Vec, range Vecs and query/tree handles. |
| Rejected publication | Current tree, injections and text remain unchanged. Previously incoming arguments dropped on return on UI. They now become the cleanup batch instead. | The same cleanup path retires the rejected result. |
| Following frame | Fold publication/notification stays at its original position. Painting queries the already-installed new tree/layers. No neutral-style frame or pending-coordinate relaxation is introduced. | Releasing old shared nodes cannot mutate the published tree; other Tree owners keep their own references. |

The cleanup batch keeps the original per-path destruction order: replaced Tree
before replaced layers; rejected argument layers before rejected Tree. The enum
variants encode this through Rust field order, without running a destructor on UI.

The Rust Tree clone copies the handle and retains the shared root. `Tree::edit`
can copy/traverse affected subtrees. `Tree::drop` invokes `ts_tree_delete`, whose
`ts_subtree_release` decrements shared references and walks/free nodes when their
last reference disappears. Thus an apparently simple assignment can perform
substantial work. The pinned implementation is in
[tree-sitter tree ownership](https://github.com/tree-sitter/tree-sitter/blob/v0.26.8/lib/src/tree.c)
and [subtree edit/release](https://github.com/tree-sitter/tree-sitter/blob/v0.26.8/lib/src/subtree.c).

Each cleanup task owns one already-existing retired/rejected tree and layer Vec;
it captures no UI entity, Rc, parser, source edit or history. There is no new
clone, global retirement collection, debounce, permit or lock. Cleanup is detached
from the parse task so replacing/cancelling that task cannot pull this batch back
onto the foreground continuation. Its finite payload is moved after publication;
its lifetime can extend until the existing background executor schedules cleanup.
This is not a new hard bound on aggregate process RSS or scheduler backlog.

## Unchanged work and limits of the claim

- The edited pending tree remains on UI with exactly its existing byte/point
  coordinates. Structural `Tree::edit` and copy-on-write costs are unchanged.
- Initial highlighter/query construction, injection snapshot cloning, render-time
  style-vector work and old fold-vector/cache retirement are not moved.
- Cancelling before background parsing begins can still drop task-captured
  snapshots on the foreground executor. Language/factory reset and highlighter
  teardown can also release their final Rc-owned syntax state there. This patch
  does not redesign those separate ownership paths.
- Reused subtrees and Arc queries can have other owners. This patch relocates the
  release of the returned owners, not every possible final release elsewhere.
- Parser snapshot construction, reuse input, parse debounce, progress cancellation,
  task replacement, source admission and fold notification order retain the same
  code. Accepted and stale-result ownership controls cover the changed boundary.
- The retained gpui-base-opt3/app-opt0 64–67 ms is aggregate synchronous
  `update_highlighter` work. Completion-time publication/retirement is a separate
  continuation. It is not a before/after result for this patch, nor a native
  key-to-paint result. No timing or reporter-acceptance target is claimed.

## Regression controls

The ownership tests use real Markdown parsing and at least 24 actual Rust
injection layers, with Hebrew, emoji, a combining character and CRLF. They retain
an independently edited pending-tree byte/point range, parser reuse metadata and
shared injection queries, and compare the published semantic spans with an
independent full parse before and after retiring the old data. The stale case
must return only rejected candidate data and retain the current tree/styles/text.

A test-only destructor meter on the actual `InjectionLayer` observes zero layer
destructions during publication/rejection and all returned layer destructions on
a separate worker thread. Deliberate local destruction fires the same meter as a
positive control. These are focused ownership/unit controls, not device affinity
or end-to-end GUI performance measurements. The application wiring moves the
same Send-only batch into its background executor and is covered by compilation.

The negative control restores the old assignment/argument-drop implementation
behind the new return ABI, returning an empty batch only to keep the caller
compatible. It executes the actual old foreground destructors; both ownership
cases must fail at their unchanged foreground-destruction assertions. A compile
failure is not a passing negative control. Shipping source and vendor lock are
restored and the complete generated stack must verify afterwards.

## Upstream plan

The owning implementation is `longbridge/gpui-kit`'s component highlighter adapter,
not GPUI core or tree-sitter. The returned-owned-data pattern is general to that
adapter and can be proposed there with these semantic/lifecycle controls after
local review. Keep it as one cumulative vendor patch while pinned. An upstream
issue/PR requires the owner's separate authorization; none is opened here.
