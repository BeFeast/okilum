# Windows installation and updates

Windows Reader uses Velopack 1.2.161. A Linux CI runner cross-compiles with
cargo-xwin and packages unsigned Beta Setup.exe/full/delta/feed artifacts with
`vpk [win] pack`. No Windows runner, Wine or user-installed .NET is required.
Since #2927 (Okilum left baldr, Oleg 14.11) the beta build and `vpk pack` run on
a GitHub-hosted runner (`windows-beta` in `.github/workflows/forgejo-windows-release.yml`).
The Forgejo `windows-release` `release` job is a `bridge` waiter
(`scripts/releases/windows_handoff.py`). As for the Arch beta, orchestration always
comes from the current workflow commit and only the product source is pinned, so an
older commit (#1040) that predates the handoff still builds. The waiter pushes the
workflow commit to a write-once GitHub ref and dispatches the hosted job on it with
the product source, the Forgejo build number (`5000 +` run number) and the embedded
Authenticode policy variables. It accepts exactly one run of that ref, proven by that
attempt's jobs, and only an artifact uploaded by that attempt with a matching sha256
digest. It then stages the result as the unchanged Forgejo artifacts. The R2 upload,
the signing decision and publication stay on Forgejo; GitHub holds no Okilum secrets
for this.
The existing Windows workflow performs one cross-build per PR/main event;
PR artifacts never publish. Main publishes Beta; [release promotion](releases.md)
supports an independently accepted Windows Stable build without rebuilding it.
Windows Stable is signed-only: the promotion gate refuses unsigned Beta artifacts
and does not sign them during promotion.

Public installer URLs:
- https://updates.befeast.com/okilum/windows/beta/Setup.exe
- https://updates.befeast.com/okilum/windows/stable/Setup.exe

Every install follows **stable** unless beta was chosen explicitly (#1307), on
both installers: the update-channel preference is separate from the downloaded
installer. Beta is an opt-in: **Settings → Updates →
Beta**, which writes `beta` to `%LOCALAPPDATA%\okilum\windows-update-channel`.
No file, or any other value, means stable. QA machines that must follow every
merge write that file before installing.
This explicit preference is retained outside the installation. Promotion uses
identical installer/package bytes and serves `releases.stable.json`; the SDK's
explicit channel setting selects the matching feed. Switching back to stable
never downgrades the current version. A downloaded update is applied only if the
channel the install follows now offers it (#1318): the package is checked against
that channel's feed and recorded in `windows-update-verified`; changing the
channel removes the record, and the next check keeps the package or deletes it.

The SDK handles installer lifecycle arguments before Reader argument parsing.
Checks/downloads run on a background thread, at startup and every 6 hours while
Okilum stays open (#683). Once a package is downloaded, a quiet **Update ready →
Restart** notification appears and stays until it is closed or Restart is chosen;
closed, it does not return for the same package until the next launch.
**Restart to update** stays available in ⋯, About and Settings, where the status
line reads «Update downloaded · restart to install». Restart protects edits and
saves Reader state before asking Velopack to apply after normal shutdown, then
reopens the same vault windows (#1325) or the standalone file.
Download alone never terminates an active Reader. Normal quit and reopen still
applies a pending update.
An offline check leaves the current version intact. Portable diagnostic builds
have no automatic updates; use an installed release for update acceptance.

Installation lives under `%LOCALAPPDATA%\BeFeast.Okilum`, separate from existing
Reader state `%LOCALAPPDATA%\okilum`. No uninstall hook deletes user state or
vaults. The Windows Reader reads, searches and edits notes with native safe
saves. Shaders and third-party notices ship with the app. Beta installers and the
Beta app are currently unsigned: Windows SmartScreen may show “Windows protected
your PC”; verify the official URL, then use **More info → Run anyway** for this
owner-approved preview. Stable requires signed artifacts and separate native
installation/update acceptance; this preview does not establish either gate.

CI restores the previous full beta package to create deltas; the hosted build
reads it from the public feed (`https://updates.befeast.com`) with the same
size and SHA256 checks, so it needs no R2 credentials. Publishing verifies
package sizes/SHA256, uploads packages first and the feed last; failure before
feed publication leaves clients on the previous release. Immutable build metadata
and Setup.exe support exact-build promotion. Failed or older publications cannot
roll the feed back. The optional `OKILUM_WINDOWS_SIGN_TEMPLATE` packager hook is
used for signed packaging; unsigned Beta publication does not establish that it ran.
Stable promotion separately verifies signed metadata and the signer fingerprint.
The unsigned Beta feed relies on HTTPS and the update host; package hashes provide
integrity, not independent publisher authentication.

## Owner acceptance on bragi

1. Install build N with Setup.exe. Select Beta in Settings → Updates. Open the
   existing vault and document; set a distinct window size and theme.
2. After N+1 publishes, use ⋯ → Check for Updates. Wait for **Update ready**;
   dismiss the toast and verify **Restart to update** remains in About/menu.
   Select Restart: About must show N+1; window/theme/current document survive.
   Repeat with an unsaved edit and a standalone file; failed draft protection
   must keep Okilum open. A second open document window must block Restart
   with a clear instruction, and closing it must permit an explicit retry.
3. Repeat offline: checking shows an error, and the existing app/vault still work.
4. Uninstall through Windows Settings. The vault and Reader user state remain.

Packaging feasibility was tested on Linux with the accepted diagnostic ZIP:
Setup/full/delta/feed in ~4 seconds; delta reconstruction matched all package
members. This does not replace native installation/update acceptance above.

Windows Beta builds the newest main once an hour; manual dispatch of
`windows-release` on main builds immediately. See [release cadence](releases.md).

## Sync supervisor (not active yet)

The package carries `okilum-sync-supervisor.exe` in the application folder (`lib/app`
inside the full package). It is a GUI-subsystem program, so a login task never flashes a
console, and the build refuses a package without it (`scripts/windows/pack.sh`,
`scripts/windows/pe.py`). Nothing starts it: Reader has no Sync on Windows yet (#1029).
When Enable exists it copies the helper out of the application folder to
`<private state>/supervisor/<version>/` (`sidecar::stage`), because Velopack replaces the
application folder on every update and a background process running from it would block
the update and the uninstall. The Beta helper is unsigned, like the Beta app;
signed Stable packaging must sign it through the same Velopack signing hook.
