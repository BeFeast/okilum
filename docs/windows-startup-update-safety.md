# Windows startup update safety (#2057)

A second ordinary Reader launch must forward its open intent to the live primary
without applying a staged update or terminating that primary. A running primary
updates only through the existing explicit restart transaction, including
`protect_all_for_quit` and state persistence.

Before Velopack startup, an ordinary launch reserves the same OS-owned instance
lock used by Reader IPC. If another primary or startup owns it, automatic apply
is disabled for this launch. An owner keeps its reservation across the SDK call
and transfers the file handle into normal IPC setup. It never releases and
reacquires the lock between deciding about updates and becoming primary.

The early pass parses launch arguments only to select the IPC profile; it does
not start a listener or GUI. Fast Velopack install/update/uninstall hooks and
non-Reader commands continue through the original SDK entry point, with automatic
apply disabled. Clipboard helper children still exit before this logic. Failure
to reserve disables auto-apply; it does not grant ownership. A changed profile
between reservation and IPC setup is refused. Explicit restart behavior and
channel/verified-package checks remain in place.

The regression tests cover reservation transfer and competing startup ownership.
The Windows SDK regression constructs an actual newer full nupkg and verified
channel record, positively checks SDK pending-update detection, and keeps a real
FileEditor draft while exercising folder, file and activation handoff intents.
It checks the original IPC identity, canonical file and draft text, installed
manifest version, and that the update remains staged. It never launches a real
updater in the unit test.

Before merging, the installed exact-PR Windows gate must additionally stage a
real verified public update, leave an unsaved UI draft, and exercise the folder
verb, Markdown association and ordinary shortcut. The primary PID/source/hash
must remain unchanged, the draft text must survive, and no update may apply
without Restart now. Capture pending-package identity and every launch's PID,
path/hash/version before assertions. Use the canonical VM150 harness from the
approved OneDrive-free snapshot, with bounded ownership and final rollback.
