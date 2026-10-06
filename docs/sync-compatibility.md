# Sync slice 1 compatibility evidence

Measured on Linux amd64 on 2026-10-06 using the reproducible
[sync enrollment commands](sync-enrollment.md). Real unchanged hub **1.29.5** and
client **2.1.6**, separate ephemeral identities and vaults, no live infrastructure.
The API version responses are asserted, independently of binary filenames.

| Check | Observation |
| --- | --- |
| Auth / identity | Correct key succeeds; wrong key fails; distinct peers; wrong expected identity rejected |
| Scoped configuration | Device creation/replay and folder PATCH read-back succeed; conflicting existing device settings and duplicate folder creation rejected |
| Reuse preservation | Unrelated folder and global GUI/options configuration remain equal after enrollment, pause and restart |
| Introduction | Client hub ref: introducer + skip removals on, auto accept off; hub client ref: introducer and auto accept off |
| Ignores before unpause | Both paused folders seeded/read back; conflicting policy rejected; ordinary canary and `.claude/skills/example/SKILL.md` arrive; index and worktree canaries do not |
| Initial receive | Receive-only downloads hub changes; locally created file stays local while a second incoming canary proves progress; receive-only change count is nonzero |
| Paths | Empty accepted; nonempty unknown rejected; known same-ID/path accepted; overlaps and changed path rejected; Unix symlink aliases resolve in unit test |
| Pause / restart | Folder remains paused across daemon restart with same identity; new canary arrives after resume; the resume is the pause probe's positive control |
| Marker error | Removing client `.stfolder` produces explicit `db/status.error` containing `marker`; boundary leaves marker absent |
| Teardown | Both directly owned daemon children killed/reaped; both REST sockets can be rebound |

The ignored two-peer integration test passed in approximately 11 seconds after
compilation. Unit tests run in the normal CI test step; the real fixture is
explicit opt-in so ordinary builds do not download or start external binaries.
Pinned release archives and hashes are in `scripts/sync-compatibility.sh`.

Two upstream details discovered by executing the fixture: 1.29.5's generate
subcommand does not support `--no-port-probing`; and `--no-restart` alone does not
remove its monitor process. The fixture uses the upstream internal `STMONITORED=1`
for both pinned binaries so `Child` owns the actual daemon. This is test harness
behavior, not the production service lifecycle design. Empty ignore lists may
read back as JSON null; an empty-policy write/read-back regression is exercised
on both binaries. Owned binary launches clear ambient environment overrides. Syncthing normalizes nested config defaults and device
ordering, so read-back compares requested fields and array members rather than
requiring byte-identical JSON.

Limits: no passkey, service grant, hub adapter, live rollout, fleet introduction
or revocation enforcement, controller journaling, receive-to-send promotion,
package/service installation, desktop UI, macOS/Windows filesystem behavior,
case collisions, disk-full or conflict recovery is proven here. Two peers verify
the introducer configuration, not a third peer's introduction/removal behavior.
The ignore fixture is policy-shaped, not a copy of private live vault policy.
Later slices must exercise these additional behaviors before claiming enrollment
or native delivery complete. Future package versions are not automatically
covered by this compatibility result.
