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
installed registry proof → visible Explorer folder-menu invocation cold → visible
folder-menu invocation warm on another folder → normal close → Update.exe
--uninstall --silent → absence checks. The default QA profile is isolated by the
clean VM. Its ordinary `vault_open=current` preference makes A→B handoff observable
in the same primary PID. No second app/open driver or command-line folder launch
substitutes for the two actual menu invocations. Each invocation validates Explorer
process/session, selected folder and unique accessible menu label, records its
menu frame, and invokes that menu item's real InvokePattern.

Silent installation deliberately avoids an intermediate launch/close that could
apply a public pending update to the low-version PR seed. This is stock pinned
Velopack1.2.161 behavior, not a network/cache/signature bypass:
https://github.com/velopack/velopack/blob/1.2.161/src/bins/src/commands/install.rs
(`if !dialogs::get_silent()` controls app startup, after install hooks).

Both Directory and Directory/Background registration commands are checked against
owned installed bytes. The native visual actions exercise the Directory item;
background placeholder/drive-root quoting additionally has a real Windows
CommandLineToArgvW regression. Uninstall must remove the two owned verb subtrees,
Markdown/URL registrations and its positively observed uninstall entry, preserve
a neighboring control verb, and retain every fixture-vault byte.

`EXECUTED_PENDING_FRAME_REVIEW` is not acceptance. Inspect every retained frame,
full source, executable/hash, cold/warm root+PID, registry before/after, unchanged
vault hashes and cleanup raw. Any first product failure stops acceptance and is
reported to Manager; tooling failures require concrete classification before any
repair. No signing, promotion, public-feed mutation, installed Oleg app, snapshot
mutation or native Mac/Linux use is authorized by this scenario.
