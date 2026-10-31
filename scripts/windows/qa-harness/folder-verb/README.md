# Canonical installed-PR folder verb gate (#1914)

This committed fixed scenario is deployed unchanged under the canonical VM150
harness as `folder-verb/` and committed there before execution. Native execution
from the product checkout is refused. `pr_seed.py` is the unchanged canonical
original-artifact admission from070d5ed: successful exact-head hosted run, original
unsigned-windows artifact digest, exact Setup bytes and package identity. The
installed executable must match the executable inside that original package.

After an explicit Manager grant and source review:

```
python3 folder-verb/run.py --execute /absolute/manifest.json /absolute/new-evidence 'exact Manager grant'
```

Manifest fields are `source`, `hosted_run`, `artifact_metadata`, `artifact_zip`,
`setup`, all retained paths except the full source SHA. No native action is
implied by creating or validating a manifest. Announce the printed owner/slot to
the current UI executor and check reservations before entering literal `FREE`.
The runner holds the foreground GUI flock and canonical owner/expiry lock,
rolls back to the pinned clean-noonedrive-v2 before/after, preserves raw failures,
and requires an independent postclean app/task0 readback. Maximum work45minutes
plus15minutes cleanup, within the original attempt deadline.

The native sequence is controls → quiet session → original Setup --silent →
installed registry proof → cold Directory `FolderItem.Verbs()/DoIt()` → warm
Directory dispatch on another folder → normal close → Update.exe
--uninstall --silent → registry and shell-verb absence checks. The default QA
profile is isolated by the clean VM. Its ordinary `vault_open=current` preference
makes A→B handoff observable in the same primary PID. The driver resolves the
exact selected folder through `Shell.Application.Namespace(parent).ParseName`,
retains every exposed verb name, requires exactly one matching label and calls
that verb's `DoIt()`. No direct executable/command-line launch substitutes for it.

Manager's r5 disposition accepts this real shell dispatch as the runtime gate.
Shift+F10 menu frames and UIA census are supplementary evidence; visualization
failure never dispatches or blocks the runtime test. The selected folder owns
keyboard focus before Shift+F10. Windows11's Show more options is expanded when
observed, using visible MenuItem/Button controls owned by Explorer.

`test-wait-scope.ps1` exercises the actual polling function with a caller UIA
condition sentinel and a negative timeout control. Its scriptblock parameter
must not shadow the caller's condition in PowerShell dynamic scope.

Tooling continuations within an existing grant must set
`OKILUM_QA_RELEASE_EPOCH` to the original absolute release epoch. The runner
only shortens deadlines and reserves15minutes for cleanup; it refuses admission
with fewer than15minutes of work remaining.

Silent installation deliberately avoids an intermediate launch/close that could
apply a public pending update to the low-version PR seed. This is stock pinned
Velopack1.2.161 behavior, not a network/cache/signature bypass:
https://github.com/velopack/velopack/blob/1.2.161/src/bins/src/commands/install.rs
(`if !dialogs::get_silent()` controls app startup, after install hooks).

Both Directory and Directory/Background registration commands are checked against
owned installed bytes. The runtime dispatches exercise the Directory item;
Directory/Background runtime is NOTRUN and remains a #1914 follow-up. Folder
Self verb census is retained separately and does not prove Background dispatch.
Background placeholder/drive-root quoting additionally has a real Windows
CommandLineToArgvW regression. Uninstall must remove the two owned verb subtrees,
Markdown/URL registrations and its positively observed uninstall entry, preserve
a neighboring control verb, and retain every fixture-vault byte.

`EXECUTED_PENDING_FRAME_REVIEW` is not acceptance. Inspect every retained frame,
full source, executable/hash, cold/warm root+PID, registry before/after, unchanged
vault hashes and cleanup raw. Any first product failure stops acceptance and is
reported to Manager; tooling failures require concrete classification before any
repair. No signing, promotion, public-feed mutation, installed Oleg app, snapshot
mutation or native Mac/Linux use is authorized by this scenario.

For r6, before installation/GUI launch the snapshot-local hosts file maps
updates.befeast.com to unspecified IPv4/IPv6 loopback. The resolver must first
return a public address (positive control), then only blocked addresses. The
original hosts file and resolver readings are retained; canonical rollback
restores them. No product update setting, signature, or feed content is changed.
Cold/warm capture identity before guards (path, PID/start, SHA256, file/product
version, source, package XML and process census). UIA Projects is the positive
control for the Update ready/Restart now absence assertion; no verified pending
package record may appear. The executable hash must still match before uninstall.
