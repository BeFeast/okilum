# Windows downloaded-update affordance (#683)

Proposed bounded implementation; this document does not enable the behavior.
Installed Windows releases should announce a completed download once, with a
quiet bottom overlay: **Update ready** and **Restart**. Dismissal does not discard
the package. About and the existing Check for Updates entry retain access to the
same ready state. Linux continues to use pacman; macOS retains Sparkle.

## Existing behavior and implementation boundary

`updater/windows.rs::check(false)` downloads on a background thread but discards
its result. Manual checks use a Win32 message box. `get_update_pending_restart()`
already identifies a downloaded package, so readiness must derive from that
package, not from a successful feed check or an available-but-incomplete download.

Introduce a small platform-independent update state model with a Windows backend:
checking, downloading, ready(package identity), failed, idle. Publish background
results onto the GPUI thread and refresh windows. Deduplicate notifications by
package identity across checks and windows; render in the active Reader. A later
Reader must still observe readiness if the download completed before it existed.
Keep readiness after toast dismissal and clear it only when the package is no
longer pending. Never run package or network I/O in a render callback.

Use the existing bottom notification layer, without shifting document layout or
creating a second modal. Give the ready notification a stable identity and an
explicit dismiss action; do not replace a Trash/Undo action or a recovery notice.
About and Check for Updates show **Restart to update** when ready. Errors remain
visible on explicit interaction; failed background checks do not spam notices.

## Restart transaction

1. On explicit Restart, serialize the request and revalidate the pending package.
2. Run `reader_editor::protect_all_for_quit` for every open editor. If protection
   fails, remain open and show the failure; do not arm the updater or lose edits.
3. Persist normal window/vault/note/history state and prepare supported relaunch
   arguments. Preserve single-file launches and normal multi-window restoration;
   do not pass obsolete installer flags or assume one active vault is all state.
4. Use Velopack `wait_exit_then_apply_updates(..., restart=true, args)`, then normal
   GPUI quit. Its immediate `apply_updates_and_restart` helper calls `exit(0)` and
   would bypass normal shutdown. The wait helper itself times out after 60 seconds,
   so complete blocking preparation before arming it.
5. If arming fails, stay open with ready state intact and permit an explicit retry.
   A duplicate click must not spawn another updater. No automatic forced exit.

## Delivery and evidence

- PR 1: state model, Windows event delivery and guarded restart, tested with an
  injected updater. Cover download failure, ready before first window, duplicate
  checks/clicks, multiple windows, dismissed toast, pending-package disappearance,
  editor protection failure and updater-arm failure. Assert call order.
- PR 2 only if needed: shared ready UI and restoration gaps found in PR 1.
- Linux light/dark screenshots use an injected ready state and are labelled UI
  fixtures; Linux must never expose a functioning Windows restart action.
- Windows cross-build validates compilation; native installed N -> N+1 acceptance
  must verify draft/vault/note/window restoration, failure recovery and one restart.
  A Linux fixture or portable ZIP is not evidence of successful Velopack apply.
- One PR-Agent review per implementation PR, required CI and normal release flow.
  No changes to M4, its VPN window, installer signing or release cadence.

Estimate: 1–2 executor days plus installed Windows QA. The critical gate is a
normal safe quit after arming the updater, not merely showing a clickable toast.
