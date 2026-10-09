# Native GUI V2 and brand adoption

Tracking: [implementation #101](https://git.oklabs.uk/BeFeast/okilum/issues/101).
The implementation applies the approved design to the Rust/GPUI application.
The browser prototype remains a design reference, not the application runtime.

## Approved sources

| Source | Version and identity |
| --- | --- |
| Canonical logo | Open join B, brand asset release **1.2.0**, brand source commit `7bf512a` |
| Brand typography and semantic foundations | Immutable tokens **1.1.0**; Noto Sans and Cascadia Code |
| Application chrome | GUI interface aliases **2.0.0**, neutral light/dark surfaces and blue actions |
| Layout and interaction reference | [Design PR #100](https://git.oklabs.uk/BeFeast/okilum/pulls/100), exact source `3ba23948adbabb4288bb3bc6fa0d3e4e074861e2` |
| Product baseline | `a68a7d15fa435a06445641d4aabb6d96994fc112` |

The GUI aliases explicitly supersede the earlier navy application surfaces.
The outlined wordmark is artwork, separate from the runtime font stack.

Downloaded source archives were independently verified on 2026-09-06:

- [Brand 1.2.0](https://pomoi.co/f/3b1f7e6d-1a7a-4031-b92f-06c1474770b7/okilum-brand-identity-1.2.0.zip):
  SHA256 `98778c01dc99387c01d2bf45a0374d6ea6aa15d4dcf8c4ffebfb9e1f8935ba58`.
- [GUI V2](https://pomoi.co/f/64d42448-7195-4c22-bf73-04d018494497/okilum-gui-v2.zip):
  SHA256 `1acc4fa83d8e7b86389383983b74fc94461a9ee718f8a1a51b1e27cbecf4be6e`.

## Integration contract

The desktop keeps navigation, the goal/source list and selected detail visible
together. Discussion uses a docked composer and independently scrolling content.
Execution, outcome, history, exact source editing, preview and conflict recovery
remain connected to the selected goal and existing guarded APIs.

Retained entities preserve goal ownership, input focus and drafts across polling
and navigation. Reader stays read-only. Source writes retain revision checks and
raw Markdown preservation; export contains saved knowledge, not unsaved drafts or
operational replay state. Connector controls do not imply provider availability.
No prototype-only provider capability becomes a production action.

## Verification scope

This is P1 for changed navigation/composer behavior and P2 for appearance.
Verification uses existing regression tests, a single correctness review and
bounded native light/dark reference scenes with disposable data. It does not
repeat the live provider acceptance exercise or change its previous verdict.
Browser design checks do not establish native accessibility or live acceptance.

Implementation and final build evidence are recorded in issue #101 as the
combined source becomes available. Installed/deployed and user acceptance stages
must be reported separately from tests and packaging.

## Prepared-stage recovery

Execution offers Revise stage and Discard stage only when the backend reports a
never-sent preparation. Revision edits the next step while retaining frozen
sources, criteria and result ancestry. Discard requires an in-app confirmation;
prior stage/context records stay accessible in history. Start carries the exact
prepared guard displayed by this client and stays disabled while editing or
recovering a change. Legacy backends keep their prior Start behavior.

Unsent revision drafts belong to the selected goal and survive navigation in the
current desktop session. Once submitted, the exact operation UUID, request and
workspace identity are saved under `okilum/prepared-operations/<brain-id>` in the
local configuration directory before any request is sent. Reopening that saved
workspace restores pending recovery. These operational records are separate from
canonical Markdown and the disposable index; they are not exported as knowledge.

A lost acknowledgement retains the same request for an explicit retry. Only a
matching receipt or the backend's explicit durable non-recording proof ends the
pending operation. Immutable terminal markers clear only that operation, including
when another desktop window has newer work. A stale editing draft is preserved;
applying it to the current prepared stage requires an explicit UI action.

## Workspace Attention

Attention reads a workspace-wide backend projection independently of the selected
view. Each row retains its goal, attention and recorded stage identities and
shows its goal title, kind and message. Search covers titles and messages. The
observation timestamp means backend state was inspected; it does not claim a fresh
provider check. Failed refreshes retain the last list with an explicit stale
warning. Missing backend capability is unavailable, never an empty queue.

Selecting a row re-reads its owning goal without changing saved selection or
launching work. Dirty source navigation uses the existing save/discard boundary;
conversation drafts and pending prepared changes remain goal-owned. A resolved or
superseded row opens goal history, retaining the observed message and its stage or
result link where the backend recorded ownership. Unknown legacy ownership stays
unknown. Workspace Attention refreshes every five seconds while visible or while
another goal has durable running work; selected active work retains its existing
one-second refresh. Idle views outside Attention do not add polling.

## Read-only Reader side panels (#321)

Notes/navigation opens on the left; Backlinks opens on the right. At 1000px
and wider both can remain visible and each closes independently. The document
retains at least 480px; excess preferred panel widths are temporarily clamped
without changing their saved values. Below 1000px the most recently opened side
is an overlay at its corresponding edge. Compact visibility does not erase the
other side's wide-window choice. Closing the compact overlay exposes the document.

Both native toolkit resize handles update separate preferred widths in the app
configuration directory, outside the canonical vault. Saving one side merges the
other side's latest saved width. The document entity and element position stay
stable across panel actions and viewport transitions, retaining selection, scroll
and navigation history. Reader opts its panel controls and splitter hit areas into
preserving selection for their pointer gestures. Its TextView retains settled
inline byte ranges through layout changes; ordinary document/search clicks and
content replacement retain their selection-clearing behavior. Patch0019 provides
these opt-in toolkit seams without changing GPUI core. Hidden Notes input cannot
retain focus after compacting.

The initial right-only composition at `8c7ae302` was rejected during native g10;
its component resize evidence is not acceptance of the final composition. Native
acceptance of corrected left/right topology and simultaneous wide panels is a
separate gate before merge or delivery of a Mac composition pin.
