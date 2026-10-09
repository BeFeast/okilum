# Protecting drafts on normal window close

The managed AI Brain window vetoes a normal window close while the latest visible
Markdown draft still needs local durability confirmation. It freezes editing and
navigation, protects the newest coalesced text, and removes only that exact window
after acknowledgement. Repeated close requests retain one pending intent.

The intent binds the complete workspace identity, original source snapshot, exact
text and active recovery record identity. A late response or another window's
changed recovery generation cannot silently redefine that intent. An existing
active generation is re-confirmed even if its visible text has not changed since
the last acknowledgement; an earlier acknowledgement is not a lease on a shared
record. Local storage
failure leaves the window open with **Keep editing** and **Retry protection**.
Retry re-lists and compares the same local generation; it never replays a retained
uncertain canonical Save. Keep editing cancels the close intent without erasing
recovery errors or uncertain operations.

An explicit Save or merge adoption which was already running when close was
requested may finish. Only the captured Save request's matching acknowledgement
can advance the bound base. Only the already-running adoption's exact protected
candidate can advance the bound text. A failed or uncertain operation keeps the
window open until protection is re-established. A normal close does not start a
new canonical Save, resolution, merge adoption or engine operation.

An already explicit **Discard changes** freezes typing and has already chosen to
retire that local text; it does not require a new protection pass to close. If
closing interrupts its background work, the old recovery record may remain for
later inspection. The close hook does not discard additional drafts.

The hook is registered only for managed `BrainView` instances, including the
ordinary workspace-profile route. It does not alter the legacy/read-only vault
window. Other windows remain open. The implementation uses GPUI's normal-window
close veto and `remove_window`, never application-wide `quit`.

This section describes ordinary window close. The application-owned Quit action
has a separate coordinator described below. Forced termination, SIGKILL, crashes,
and OS/Dock termination still recover only the last acknowledged local generation;
GPUI's late Quit observer cannot provide a pretermination veto. Canonical Markdown
and export behavior remain unchanged.

Validation includes coalesced latest typing, two-window isolation, storage-lock
failure and retry, cross-window CAS divergence, uncertain Save without replay,
legacy close behavior, and closing during an already explicit Save/adoption.
Native acceptance additionally uses a fixture-only `fsync` barrier to observe the
window remaining open while actual protection is pending, then exact newest-text
recovery after release and restart. The candidate binary is unchanged by that
fixture; no ordinary launcher or global process environment receives the shim.

## Application-owned Quit

**Quit Okilum** in the application menu and the platform secondary-Q binding
(Cmd-Q on macOS, Ctrl-Q elsewhere) enter a process-wide draft coordinator. The
coordinator freezes the current window set and registered managed editor identities.
It asks every editor to protect its exact latest generation using the same close
barrier above. Clean editors also stay frozen while another editor is pending.
No window is removed by this action while protection is pending or has failed.
Only after every captured editor is ready does the coordinator call `App::quit`,
once. Repeated Quit requests reuse the pending intent.

**Keep editing** cancels the whole Quit intent and releases every captured editor,
without clearing errors or deleting recovery records. **Retry protection** rechecks
the failed editor's captured generation. It never starts or repeats an uncertain
canonical Save. A newly registered window cancels the pending intent; the coordinator
also rechecks the full window set before quitting. Normal individual window close
is vetoed for managed editors while application Quit is pending.

This is an application-owned action, not an OS termination hook. Dock Quit, direct
NSApplication termination, logout and forced process termination are outside the
implemented interception. No GPUI core patch is used. Actual macOS keyboard/menu
routing still requires an M4 native check. The shipping shell currently opens one
window per process; supplementary same-process GPUI tests exercise two managed
editors plus a legacy window without adding a product multiwindow control.
