# Lossless first Sync (#2043)

The goal joins two existing folders through the approved paired channel. The
ordinary empty-folder attachment API stays strict: constructing a preview or
adapter never enrolls its destination or publishes canonical bytes. Settings
keeps layout A and adds the review/confirmation inside the existing flow.

## Transaction boundary

The complete inventory binds the paired identities, authorized source/root,
canonical ignore policy, selected managed runtime and both byte revisions.
`Plan` classifies Same, Only here, Only there and Different; consent belongs to
that exact plan. The existing warning requires acknowledgement for little path
overlap. Unsafe/incomplete inventories refuse instead of truncating a preview.

After confirmation, a unique operation stores its immutable manifest and
content-addressed preimages under the private controller's `first-sync/`, outside
both vaults and their deletable indexes. Metadata and blob chunks are bounded,
checksummed and replay-checked. The remote computer verifies the exact same union
from both originals. Staging and preparation never publish a canonical file.

Local Okilum writers drain under the durable per-vault authority. The ordinary
destination retains its runtime's configuration/launch gate and proves that no
configured transport folder overlaps it. The configured source composes local
drain with a selected-folder transport proof. Other folders keep transferring.
Config admission refuses a retained gate instead of waiting while holding the
pairing database, so another paired request can complete or recover the merge.
First-merge requests never exchange device addresses or transport configuration.

For pinned Syncthing v2.1.6, `lib/model/model.go:512` waits for the folder runner's
`StopAndWaitChan` before cleanup; `CommitConfiguration` emits `FolderPaused` at
line 3035 after that wait. A unique temporary folder label binds that event to
this operation, including when an older event reaches its buffer late. The
pre-request cursor and intent are durable before PATCH. PATCH readback alone
never grants quiescence. The managed generation also binds the exact live owner,
child process, executable, REST socket and authenticated REST start time.
Already-paused startup recovery uses the launcher's recorded no-runner fence.
The original label is restored on authorized resume.

Both roots revalidate under those capabilities before the first canonical write.
Publication preserves originals in the private journal first. Replaced canonical
inodes move to unique ignored `.okilum-save-first-sync-*` names, and new bytes
publish with exclusive staging, NOREPLACE and directory fsync. Different files
keep a deterministic collision-safe extra version, with its original extension
and a recognized first-Sync conflict marker. Existing conflict inventory, note
banners and explicit resolution can discover both versions. A captured unexpected version
stops publication and remains retained; it is never silently overwritten.
Per-file publication gaps recover only against the exact saved original/target
bytes and root authority. Third versions require review.

Both exact union readbacks are durably acknowledged before transport/local writer
admission opens. A completed merge receipt is the only entrance that can enroll
this non-empty destination. Its folder and transfer intent have operation-scoped
ownership and retained witnesses; ordinary unknown non-empty attachment remains
refused. Completed journals remain durable evidence and are never automatic
authority to recreate a later deleted note.

Drop, disconnect, cancellation, an expired preview or a process crash never
release an unfinished hold. Recovery is an explicit operation against its saved
manifest and both preimages. Startup/ordinary refresh must surface pending work,
not silently apply it or enable Sync.

## Verification and remaining integration

The implementation branch adds explicit pinned-runtime controls to
`sync-managed-admission.sh`: a separate holder process proves scoped transfer
exclusion, unrelated-folder progress and recovery across holder/runtime restart;
a two-managed-instance fixture checks preview counts, no-consent/stale controls,
binary chunks, preservation of every original byte version, equal final maps and
both later transfer directions. Nonzero parent verdicts and fixture kill/reap
remain mandatory. An opt-in fixture feature also interrupts the actual application process at
durable staging, freeze, capture/publication and handoff checkpoints, restarts
both managed runtimes, then explicitly recovers the same operation. A compiled
negative control removes capture-gap recovery and must fail at that actual gap.
These controls are under verification, not release evidence.

This document describes the Linux implementation in progress. Hosted
verification of Settings and interruption controls, Mac/Windows implementation
and native beta acceptance remain part of #2043. No source merge or passing component
test alone closes its completion issue.


## Native Mac integration under verification

Fresh signed Mac preparations now create the same durable folder/launch registry
before native registration. The existing supervisor takes its process lock and
configuration gate, fences held folders offline, and records the captured child's
Darwin birth/parent/executable and exact REST-listener ownership. A small C shim
uses the SDK's `libproc` structures; no copied ABI layout or process-name matching
supplies authority. Shared inventory, merge and layout-A review now compile on
Mac as well. APFS UUID/inode/birth witnesses persist root identities across boots.

An explicit Connect/Share upgrades a running pre-registry supervisor through
its signed IPC Stop and a captured kernel exit. Only that verified retirement
can grant a fresh one-use launch generation. The device certificate, selected
signed v2.1.6 payload, configuration, folder IDs and direct pairing journal stay
in place. The new launch reference is pinned in the preparation receipt;
missing/replaced authority requires recovery rather than regeneration.
A codesigning digest change in the newly installed bundle does not replace the
retained, independently signature-verified runtime. Native crash recovery and
installed-beta acceptance remain under verification; this is not a shipping claim.

### Isolated native acceptance boundary

A Reader profile override alone does not isolate Mac Sync. Native QA must also
explicitly select `OKILUM_SYNC_QA_ROOT` before launching the app. Its `HOME` and
absolute `OKILUM_STATE_DIR` must be separate descendants of that already-private
root. Normal launches retain the account-derived Application Support location.

The isolated path uses an explicitly selected, private QA root, disjoint
from the account control plane. Reader HOME/state and all selectable vaults must
be inside it; vaults belong under its `vaults/` child and cannot overlap control
state. The same installed signed app verifies and starts its own signed helper
and pinned payload against this separate journal. It performs no SMAppService
registration or account-state migration. Its launch still consumes the existing
one-use permission, proves the calling app's parent identity, and serves the
same signed IPC and first-merge protocol. A normal app relaunch can reconnect to
that retained generation; explicit authenticated Stop owns fixture cleanup.
The helper accepts the separate root only through an inherited channel from
its actual signed parent app; a standalone `--state` path cannot select it.
Controller/configuration selection and vault opening refuse account or external
paths. `okilum sync qa-stop` in that same QA profile records Disable, authenticates
its exact helper generation, requests Stop and waits for the captured kernel exit
before recording completion. It does not unregister, signal or delete account
state. Native hosted controls cover the parent and path refusals. Installed-beta
acceptance remains pending.

An interruption after durable verified retirement can resume unregister,
registry binding and the next one-use registration. An interruption before the
native exit proof is committed remains recovery pending; a missing endpoint or
process name never reconstructs it. Existing paired identities, configuration
and canonical files must remain intact. Installed-beta upgrade and first-merge
release acceptance remain pending.

### Windows integration under verification

The shared direct controller now compiles on Windows through retained NTFS
root handles, protected journal/blob staging, revision-aware ignore publication,
and no-replace first-merge capture/publication. Native launch ownership uses
process creation times, captured parent/image identity and the kernel TCP owner
tables for the exact authenticated REST endpoint. The supervisor must retain its
Job Object before publishing that generation. Runtime preparation, production
Authenticode policy, pinned Windows payload packaging and Settings integration
remain unshipped until their native acceptance passes; this is not a beta claim.

The 600-second hosted managed-acceptance deadline remains unchanged. Thirteen
independent crash checkpoints were taking roughly 40–57 seconds each on the
hosted runner, exhausting that deadline before the remaining controls ran. The
runner now uses two independent test processes and private state roots, requires
every compiled parent test and all thirteen ordered checkpoint acknowledgements,
and still rejects and reaps survivors. No scenario or negative control is removed.

Windows production trust follows `sync-sidecar-discovery.md`: the compiled
publisher subject and complete native issuer chain come from release variables,
not a renewable leaf-certificate thumbprint. WinVerifyTrust supplies the trust and
timestamp verdict; the same retained file is compared with that policy. Native
controls require a trusted signed positive image and refuse a different publisher,
a different chain, an unsigned image and changed executable bytes. The release
packager still verifies every shipped PE against its configured signing key.
The pinned Syncthing executable body is checked independently of its replacement
Authenticode certificate table, so re-signing cannot substitute another program.

Windows retains the documented Task Scheduler restart policy. Each captured Job
Object generation takes the process lock and fences closed folders offline before
spawn; it does not reuse macOS's SMAppService one-use registration rule. The
runtime starts with `STMONITORED=1`, so its captured root owns the authenticated
REST listener rather than an extra Syncthing monitor. Native Job Object controls
exercise that fixed environment as well as complete descendant termination.

Windows preparation now has its own durable offline phases before registration:
allocation, staged signed package, in-flight identity generation, generated identity,
configured defaults and retained launch reference. Retrying Ready verifies the same
certificate/configuration/runtime and never reconfigures paired folders. An
incomplete identity generation stays visible and never reruns `generate` to mint
another identity. Signed helper/runtime and MPL provenance are retained outside
updater `current`. The native preparation control runs the real pinned generator
under isolated USERPROFILE/LOCALAPPDATA. Task/Connect and Settings composition
remain pending; preparation code alone does not enable the Windows user flow.


Windows Settings now uses the same layout-A review and first-merge controller.
Invitation creation remains independent of runtime startup. Explicit Share or a
first receive connects the signed installed payload through offline preparation
and the native Task Scheduler adapter. Portable distributions surface the missing
helper instead of starting another runtime. The existing account-local Sync and
SyncDirect paths remain the identity locations; no journal is migrated into the
rebuildable index. Windows does not expose the Unix-only `okilum sync` CLI/control
socket. Its running desktop owns the private direct-controller lock.

The native signed-task control uses only a fresh GitHub-hosted Windows VM/account.
A temporary fixture certificate exercises the production Authenticode policy;
it does not enable same-user development trust or access release signing. The
control leaves the actual scheduled helper running while its source package is
removed, then checks the same generation, certificate/key hashes and paired
journal identity through a newly located signed app. Stop captures the exact
server process object from its authenticated pipe before the request, requires
complete owned-Job shutdown, and waits for that process's kernel exit. Re-enable
must preserve identities and produce a fresh generation. These are acceptance
controls, not a statement that Windows beta delivery has occurred.

Its temporary public root uses the disposable VM's LocalMachine store and is
removed by exact thumbprint; the leaf private key remains in CurrentUser My and
is deleted after Stop. Current-user root imports require an interactive trust
confirmation even with certutil's force-overwrite option, so the hosted fixture
uses its existing elevated token for machine-root installation. Current-user
trust inherits machine roots as described by [Windows certificate-store
scopes](https://learn.microsoft.com/en-us/windows-server/identity/ad-cs/inspect-certificates-manage-certificate-stores).
No app runtime or release signing path installs this fixture trust.

The supervisor releases Task Scheduler's inherited state CWD before opening its
journal: Windows retains a directory read/traverse handle for that CWD, which
would conflict with the store's exclusive directory transaction. It selects its
own executable directory and continues using absolute state/configuration paths.
The historical task XML remains unchanged. A native binary test starts from the
actual state working directory; a compiled negative control removes only that
startup transition and requires the real store-busy refusal. No store sharing or
ownership gate is relaxed.
