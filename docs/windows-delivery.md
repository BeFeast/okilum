# Windows installation and updates

Windows Reader uses Velopack 1.2.161. The Linux CI runner cross-compiles with
cargo-xwin and packages unsigned Setup.exe/full/delta/feed artifacts with
`vpk [win] pack`. No Windows runner, Wine or user-installed .NET is required.
The existing Windows workflow performs one cross-build per PR/main event;
PR artifacts never publish. Main publishes beta; the [unified releases workflow](releases.md)
promotes the matching builds on all three platforms without rebuilding or signing them.

Public installer URLs:
- https://updates.befeast.com/tessera/windows/beta/Setup.exe
- https://updates.befeast.com/tessera/windows/stable/Setup.exe

Beta is the default update preference, matching the packaged beta channel and
the first published feed. To receive only promoted releases, select
**Settings → Updates → Stable** after a stable release is available.
This explicit preference is retained outside the installation. Promotion uses
identical installer/package bytes and serves `releases.stable.json`; the SDK's
explicit channel setting selects the matching feed. Switching back to stable
never downgrades the current version. An already downloaded update still applies
at the next launch, even if the channel preference is changed afterwards.

The SDK handles installer lifecycle arguments before Reader argument parsing.
Checks/downloads run on a background thread. **⋯ → Check for Updates** (also in
About and Settings) reports errors or a downloaded update. Quit and reopen to
apply it; the updater does not terminate an active Reader automatically.
An offline check leaves the current version intact. Portable diagnostic builds
have no automatic updates; use an installed release for update acceptance.

Installation lives under `%LOCALAPPDATA%\BeFeast.Tessera`, separate from existing
Reader state `%LOCALAPPDATA%\tessera`. No uninstall hook deletes user state or
vaults. Windows Reader remains read-only. Shaders and third-party notices ship
with the app. The installer and app are currently unsigned: Windows SmartScreen
may show “Windows protected your PC”; verify the official URL, then use
**More info → Run anyway** for this owner-approved testing phase.

CI restores the previous full beta package to create deltas. Publishing verifies
package sizes/SHA256, uploads packages first and the feed last; failure before
feed publication leaves clients on the previous release. Immutable build metadata
and Setup.exe support exact-build promotion. Failed or older publications cannot
roll the feed back. The optional `TESSERA_WINDOWS_SIGN_TEMPLATE` packager hook is
unset until a signing service/certificate is configured; no signing secret exists.
The unsigned feed relies on HTTPS and the update host; package hashes provide
integrity, not independent publisher authentication.

## Owner acceptance on bragi

1. Install build N with Setup.exe. Select Beta in Settings → Updates. Open the
   existing vault and document; set a distinct window size and theme.
2. After N+1 publishes, use ⋯ → Check for Updates. Wait for “Update downloaded”,
   quit normally and reopen. About must show N+1; window/theme/document survive.
3. Repeat offline: checking shows an error, and the existing app/vault still work.
4. Uninstall through Windows Settings. The vault and Reader user state remain.

Packaging feasibility was tested on Linux with the accepted diagnostic ZIP:
Setup/full/delta/feed in ~4 seconds; delta reconstruction matched all package
members. This does not replace native installation/update acceptance above.
