# AppImage in-app updates (#1470, slice 3)

An AppImage launch is identified by `APPIMAGE`. Settings and About use the existing
channel selector, inline check result and Restart to update action. Stable is the
default; selecting Beta is explicit and persisted outside the disposable index.
An ordinary Linux package continues to use its package manager.

The updater downloads `https://updates.befeast.com/okilum/linux/{beta,stable}/latest.json`
and its detached signature. The public key is the existing Linux authority
`7FFE9F27ECC8E253E45BAEA08AACBFB2C9E47882`, compiled into the updater. Gpgv is bundled
in the AppImage; its private verification homedir excludes the user's keyrings.
HTTPS redirects and alternate origins are refused. A descriptor must match its
channel, schema, platform, build/version, full source SHA, size, SHA256, authority
and exact immutable payload key. The payload's detached signature, size and hash
are checked independently. Provenance is authenticated by the signed descriptor's
hash and must agree on source/build/version/payload hash. Only newer builds apply.
An absent Stable feed reports a failed check and retains the installed file;
it never falls back to Beta.

The original absolute user-owned regular writable executable is the target.
Symlinks, hardlinked installs and unwritable destinations require a manual update.
An actual temporary-file create checks the parent directory. The verified payload
is staged in a private sibling directory on the same filesystem, retaining regular
permission bits and stripping special bits. A per-target flock serializes updates.
No root, host configuration or vault source writes are involved.

Restart first collects supported relaunch arguments, protects editors and persists
window state using the existing restart contract. It then arms a bounded external
helper and quits through GPUI. The helper inherits the lock, waits up to 120 seconds
for the original process to stop, rechecks the original inode/hash and staged hash,
then atomically renames the payload over the original pathname and flushes storage.
It relaunches the AppImage through its extraction fallback and forwards arguments
without shell interpolation. The child closes the inherited transaction descriptor.
A compiled build/source/hash confirmation after GPUI has an active window commits
the transaction and releases the helper lock. The confirmation is bounded to 60
seconds; if the application remains open without confirming, the helper leaves it
open and retains the previous copy for explicit recovery. Exit before confirmation
restores the old executable only if installed bytes still match the replacement.
A later session exit never rolls back a committed update. The recovery directory
is printed in the diagnostic log; it contains `previous.AppImage` and `apply.log`.
A successful replacement retains the previous file for manual recovery. Restore
that file to the original path while Okilum is closed. A crash before arming leaves
the installed file intact; a shutdown timeout refuses the replacement. Crash
recovery does not silently install an unconfirmed/stale staged download.

## Hosted and native acceptance

`cargo test -p okilum-appimage` exercises malformed descriptors, target links and
permission checks, target changes, actual rename/rollback, child launch failures,
and production rejection of a real disposable-key signature. Negative probes have
positive controls. These tests are transaction controls, not native build proof.
The regular hosted lane still builds one AppImage and runs it on Ubuntu and Fedora;
the bundled gpgv gets a load/execution control too.

PR AppImages alone enable `appimage-acceptance-harness`. Main/release artifacts do
not compile its CLI entry, and production publication rejects harness provenance.
The explicit `--appimage-update-acceptance` entry accepts a local fixture directory,
disposable fingerprint and receipt path; it cannot change normal updater trust.
It first proves that the actual executable's production verifier rejects fixture
trust, then verifies the fixture descriptor and payload with the explicitly supplied
public-only key. It shares production staging and apply code. The real newer
executable must confirm its own compiled build/source after restart and record its
installed hash. Acceptance requires genuinely distinct increasing build/source
identities. Fixture trust is never selected from an environment variable.

Preserve artifacts from two meaningful implementation/review commits, each compiled
once. Native pre-merge proof must update actual N to actual N+1, check the restart
receipt and original-path hash, and include tamper/failure controls. No version
relabeling, rebuilt source artifact, production private key or moving feed change
is part of this test. This document describes the gate; it does not claim a run.
Production signed beta verification remains separately reported evidence.

## Review 3788 follow-up

Initial head `028474c0` is not a working native baseline: review identified Linux
restart definitions gated to Windows/tests and a live-executable write-open that
fails with ETXTBSY. The fixed source expands the Linux definitions and proves the
kernel ETXTBSY control without write-opening the target during detection. A
long-lived-child regression verifies that the helper commits and another update
can acquire the lock while that child remains running. A real-editor GPUI test
uses the production restart preparation function, refuses a failed recovery-journal
write, retains the latest editor text and succeeds after recovery storage repairs.
These new regressions must execute in hosted CI; local compile checks are not PASS
execution evidence.

The native driver is `scripts/appimage/update-acceptance.py`. It consumes existing
older/newer artifact directories, checks actual executable build facts, signs the
fixture with an ephemeral key, tests descriptor/payload tamper retention and actual
next-executable failure rollback, then verifies the real N-to-N+1 restart receipt.
Run it unprivileged in the hosted Ubuntu/Fedora runtime with loaded GUI libraries.
No compilation or downloads occur in the driver.

The first accepted artifact after these fixes becomes working N. Preserve it before
pushing a meaningful final native-UI/review commit for N+1. Native Settings channel
selection, an unsaved editor/recovery refusal, successful protected Restart and
window/document restoration must additionally be driven through the actual GUI;
the early CLI pair driver does not exercise those paths. Those native GUI controls
and the two-artifact execution remain open acceptance work, requiring independent
review; neither this document nor hosted unit tests substitute for them.

## Review 3789 follow-up (execution still pending)

The pair driver no longer accepts an identity file alone. It requires the real
startup acknowledgement, the restarted child's successful compiled-identity marker,
the helper's committed log and acquisition of that transaction's exact flock before
checking installed/backup hashes. New helpers additionally persist an atomic
`completed` acknowledgement. The external CLI identity receipt is now published
atomically after successful startup confirmation. Failure workspaces are retained;
no driver cleanup removes a live target or stage. Hosted instrument controls cover
identity without startup, startup without commit, commit while the lock is held,
full completion as a positive control, and explicit rollback rejection.

The native feature now also exposes an explicit `--appimage-gui-acceptance DIR`
route. Main/release builds omit this route. It runs the normal GPUI application,
Reader editors, native Settings/About methods and the production protected Restart
path; it does not enable `gpui-test-support` or the preview-status/channel fixtures.
Only that explicit non-shipping route may stage a public-key disposable fixture,
after first proving production rejects its valid signature. A bounded command
harness records actual windows, documents, geometry, editor text and compiled
build/source/installed hash. `gui-acceptance.py` drives Stable/Beta persistence,
About, a real failed editor recovery write and refused Restart, repair and successful
protected Restart, then actual multi-window/document/geometry restoration. With the
new GUI still open it verifies the helper lock is released and restages/discards
the exact signed current payload through production staging as a lock positive
control. That last control does not install or invent an N+2 release.

The runtime-only `appimage-acceptance.yml` workflow takes accepted old/current
producer run IDs and full SHAs, validates successful source-pinned producer results,
downloads their retained artifacts and runs the pair plus required GUI gate under
Xvfb as an unprivileged user on Ubuntu and Fedora. It contains no compile step.
The parent runs it only after accepting those producer results. Before the
workflow exists on default, push the already-built exact new source to
`forgejo-linux-acceptance-pr/**`: this starts only the runtime job, discovers its
single existing exact-source producer and uses retained run `39764166082` as the
intended N (requiring success). It never starts a new compile. Once the workflow is
on default, explicit workflow-dispatch inputs support other accepted pairs. Disposable
signer private material is removed before evidence upload; artifact copies/stages
are retained in the hosted workspace, and uploaded diagnostic evidence excludes
AppImage copies. No release feed or production signing authority changes.

**Concrete retained-artifact limitation:** `3c2532e3` has only the early CLI fixture
entry, so its normal native Settings process cannot stage a disposable-key fixture
or enter the new GUI harness. Preserve it as intended working N if its existing
hosted gates pass. It can prove the repaired transaction pair with a genuine later
artifact, but the full GUI-pair workflow must report BLOCKED for that older binary.
Do not relabel/recompile it, inject trust into its production Settings process, or
claim the CLI pair satisfies native GUI acceptance. A real GUI pair needs two
meaningful source artifacts that actually contain the native route. This new route
and all regression/runtime results still require hosted execution and independent
exact-head review; source inspection is not PASS evidence.
