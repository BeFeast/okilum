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
without shell interpolation. A nonzero relaunch exit restores the old executable
only if the installed bytes still match the replacement. The recovery directory
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
