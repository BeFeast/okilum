# AppImage in-app updates (#1470, slice 3)

## Per-user extraction (#2750)

The shipped static type2-runtime is rebuilt from a checksum-pinned upstream
archive with `scripts/appimage/runtime-per-user.diff`. Its extraction cache is
`$TMPDIR/appimage_extracted_<effective-uid>_<image-md5>_<random-suffix>` (the runtime's default
temporary directory when TMPDIR is absent). Both `APPIMAGE_EXTRACT_AND_RUN`
and `--appimage-extract-and-run` use this namespace, including the first external
launch, managed self-install/re-exec and updater restart. Existing content-only
cache directories are never reused, removed or changed. Normal FUSE startup remains unchanged. Extraction cleanup gets a positive
`nftw` descriptor budget: musl treats the upstream zero budget as a successful
no-op, leaving the cache behind after ordinary exit. `NO_CLEANUP` still explicitly
retains it. Each launch creates a private mode-0700 directory with `mkdtemp`;
ending one launch cannot remove another running payload, even for the same UID.
The image MD5 is only an extraction namespace; release authenticity continues to
use the signed SHA256 descriptor below.

`scripts/appimage/build-runtime.sh` uses the upstream static Alpine build and its
checksum-pinned dependency archives. The hosted required check additionally
builds the unchanged runtime, requires a real two-user extraction failure, then
executes startup/copy/restart/cleanup controls against the patched runtime. A
cleanup-only mutation must also lose a still-running same-UID payload before
the private-directory overlap controls can pass.

## Update behavior

An AppImage launch is identified by `APPIMAGE`. Settings and About use the existing
channel selector, inline check result and Restart to update action. The existing
`appimage-update-channel` preference outside the disposable index always wins.
Without one (#2369), the background check verifies the signed Stable descriptor.
A running build at or below Stable starts on Stable (equal builds must match the
published source). A newer build starts on Beta only after its immutable signed
`linux/builds/<build>/release.json` proves the exact beta build/source was published;
this also covers a download older than the latest Beta. Stable promotion reuses
those same archived bytes, so publication scripts need no artifact mutation.
Successful discovery records the preference once without overwriting a concurrent
explicit choice. Offline, missing or invalid publication evidence leaves the
channel undetermined and writes no preference; Check for Updates retries.
About and Settings show the resolved channel, or an undetermined state with no
channel selected. Discovery never downloads an application payload; the normal
update path still accepts only builds strictly newer than the running build.
An ordinary Linux package continues to use its package manager.

Only a Settings choice is explicit (#2455): it also writes
`appimage-update-channel.explicit`; the preference bytes stay `beta`/`stable` so
older builds still read them. An automatic Beta is kept without network. An
automatic Stable (discovery, or a file without the marker) is re-checked on every
update check: a running build newer than signed Stable whose immutable archive
proves this exact beta build/source moves to Beta. That covers a beta tester
whose build was promoted before first discovery and a beta downloaded by hand.

The managed install `~/.local/share/okilum/Okilum.AppImage` records its build in
`.appimage-build` (written on install, on a managed launch and after an applied
update's startup confirmation). An AppImage launched from another path replaces
the managed copy only when it is strictly newer or the installed build is
unknown; otherwise it just execs the managed copy. A launcher pinned to an old
download therefore can no longer undo self-updates (#2455).

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
The parent runs it only after accepting those producer results. Before the workflow
exists on default, push the already-built exact N+1 source to
`forgejo-linux-acceptance-pr/<old_run>/<old_source>/<new_source>`. This starts only
the runtime job and discovers the single existing exact-source N+1 producer. It
never starts a new compile. Old run/source must be selected explicitly; the failed
parked producer is not a default. Once the workflow is on default, explicit
workflow-dispatch inputs support accepted pairs. Disposable signer private material
is removed before evidence upload; artifact copies/stages are retained in the hosted
workspace, and uploaded diagnostic evidence excludes AppImage copies. No release
feed or production signing authority changes.

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

## Resume after #1488 / #1564

The updated branch merges main `cb2995dcc3d3009bc43e5d886e5fe4957417cb4d`
without changing the protected independent AppImage publication path. The pacman
lane remains active until the separate #1564 switch, after self-update acceptance.
No Stable promotion or pacman freeze is part of this PR.

Retained producer inspection on 2026-10-23 found:

- `3c2532e39b328f35d03ea962a4a0c1c14f285b59`, run `39764166082`,
  AppImage artifact `12420221751`, evidence `12420233109`: AppImage job succeeded,
  overall run failed; binary lacks the GUI route. Not an accepted GUI baseline.
- `db88c49ffd7690fa2b65eaaca858ac1fc50971d9`, run `39766629494`,
  AppImage artifact `12420827286`, evidence `12420259451`: AppImage job succeeded,
  overall run failed at `clippy::cmp_owned` in provenance comparison. GUI-bearing,
  but not an accepted producer under the existing validator. The comparison now
  compares the expected build string explicitly, preserving the string schema.
- Both payload artifacts were unexpired at inspection, expiring 2026-10-25 around
  11:00 UTC. Retention does not convert failed aggregate results into acceptance.

There is currently no accepted existing GUI pair. Do not rerun or recompile those
identities. The final updated meaningful source is a candidate N after its single
ordinary CI run succeeds. Preserve its artifact and producer receipts. The separate
coordinated desktop lifecycle/UI integration is a meaningful potential N+1; compile
that source once, then use the retained pair for mandatory update/signature/native
acceptance. Parent coordinates source/build ownership and runtime execution; this
child neither polls nor dispatches extra positive-control rounds. Production-signed
beta N-to-N+1 on Arch/Ubuntu and muninn no-argument launch/vault chooser/beta-to-beta
remain separate pending evidence, without changing trust or hosts.

## Desktop integration helper contract (#1564)

Decision: silent first-launch integration, with no consent dialog. CI/release owns
`okilum_appimage::desktop::{install,remove,paths}` and its bundled approved SVG.
The helpers accept an explicit absolute home; install additionally accepts the
original absolute `APPIMAGE` path. They own only
`~/.local/share/applications/okilum-appimage.desktop` and
`~/.local/share/icons/hicolor/scalable/apps/okilum-appimage.svg`. Exec uses desktop
entry escaping, a quoted original pathname and `%u`, with no shell invocation.
Install atomically refreshes changed bytes, including after an AppImage move;
unowned launchers/icons are refused. Distribution `okilum.desktop` is preserved.
No MIME/default-handler change or system integration dependency is introduced.

The launcher uses the installed SVG's absolute file path for `Icon`, so the
first window and app grid do not depend on GNOME noticing a new icon in an
already cached hicolor theme. The shell also refreshes the user hicolor cache
with `gtk-update-icon-cache` when available, alongside the desktop database;
missing cache tools do not prevent startup. Wayland app_id and StartupWMClass
remain `okilum`, including on Hyprland/Omarchy. GNOME's window tracker matches
StartupWMClass even though the AppImage launcher has a separate desktop id.

An existing visible user launcher whose main `Exec` targets the running
AppImage takes precedence over self-installation. This includes Gear Lever's
`env DESKTOPINTEGRATION=1 …` entries and AppImageLauncher's generated desktop
files. Okilum keeps that executable in place and updates that same path; it does
not copy it to the Okilum-managed XDG installation or publish another launcher.
The check parses desktop data without executing it and requires the exact
executable path, rather than assuming every file in `~/Applications` belongs to
a manager. Manager launchers/icons are preserved. A previous launcher bearing
Okilum's own ownership marker is retired when the running image acquires such
external integration.

When an older version has already made a duplicate XDG copy, updating that copy
retires only Okilum's marked launcher if a live manager entry identifies Okilum
through `Name`, `X-AppImage-Name`, and matching `Exec`/`TryExec`. The current XDG
image must have both its ownership marker and an identity-bound build record.
The existing manager image and launcher are retained. Launching the older
manager image still reaches the newer recorded XDG copy through the older
installer's existing downgrade guard; startup removes its transient duplicate
again. Fresh manager installations stay at the manager path from the outset.

The UI owner must coordinate through the parent's registry before wiring the
minimal lifecycle hook: invoke install off the GPUI thread only on ordinary
AppImage startup (not help/build-info/QA fixture routes), silently; log an error
without blocking launch. Invoke remove after existing uninstall refusal and
confirmation checks, even if the AppImage has moved, include its exact paths in
the uninstall inventory/report, and refresh the desktop database if available.
This PR delivers the payload/helper only: neither startup nor uninstall is wired
here, so first-launch integration is not claimed complete. That boundary leaves
a meaningful coordinated GUI-bearing N+1 and avoids duplicate shell ownership.

Local verification at resume: changed Rust formatting, Python/YAML/bash syntax and
whitespace checks only; no local tests/builds/containers/artifact execution.
Workspace `cargo fmt --check` is blocked by the existing vendored
`bidi_geometry.rs`; `scripts/vendor-setup.sh --verify` reports that this local
checkout differs from the complete patch stack. No vendor files were changed.
The hosted lane prepares and verifies a fresh pinned vendor before its gates.
AppImage payload/evidence retention is seven days for the accepted pair handoff.
