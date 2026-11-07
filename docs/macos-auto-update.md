# macOS releases and updates

Okilum updates itself with stock [Sparkle 2](https://sparkle-project.org) (2.10.0,
pinned by `scripts/updater/sparkle-lock.json`). The app uses
`SPUUpdater` with a public custom `SPUUserDriver`; About owns progress, errors,
Cancel update and Restart now. The metadata/freshness driver is also custom
and creates no windows. Settings → Updates offers three persisted modes:
background download (default), ask before downloading, and Off (manual only).
The background mode drives the existing silent metadata probe into the custom
download driver, which stops at Ready. Ask/Off show the available version and a
Download update button. Sparkle's own automatic installer remains disabled;
every prepared installation still passes [signed freshness and the final Rust
editor guard](macos-update-freshness.md) after an explicit Restart now click.
Ordinary Quit cancels the prepared installation through Sparkle's public Skip
continuation; quitting during extraction waits for that cancellable boundary.

The application menu has **About Okilum**, **Check for Updates…** and
**Receive Beta Builds**. Sparkle uses the public thirty-minute scheduled-check
setting with two minutes of jitter. Stock Sparkle 2.10 clamps its timer to a
minimum of one hour, so the shared scheduler also runs its silent metadata probe
every 28–30 minutes. Window focus and application activation trigger discovery
immediately, with a shared five-minute automatic-check throttle. Off disables
both native scheduling and the shared automatic gate; explicit checks still
work. No checks create Sparkle windows. See [update modes](update-modes.md).


## How a build is released

The daily 10:30 UTC newest-main build (or manual build now) runs [`macos-release.yml`](../.forgejo/workflows/macos-release.yml)
on the existing M4 runner (label `macos`):

1. build `Okilum.app` with `CFBundleVersion = 5000 + run number` and
   `CFBundleShortVersionString = 0.1.<build>` (shown in About);
2. sign with the Developer ID, notarize, staple, ZIP, check Gatekeeper on a
   quarantined copy (`scripts/build-macos-ci.sh`, `scripts/updater/sign-bundle.sh`);
3. pack the same stapled app into `Okilum-<version>.dmg` for first installs (app,
   Applications link, branded window from `scripts/macos-dmg/`), sign it with the
   Developer ID, notarize and staple it, and check a quarantined copy with
   `spctl --assess --type open` (`scripts/updater/make-dmg.sh`, #993);
4. sign the ZIP with the EdDSA key (`sign_update`);
5. upload the ZIP to the public feed, add the build to its `beta` channel, and keep
   a copy as Forgejo release `macos-stable-<build>` (`scripts/updater/release.py`).
   The DMG goes to the same release, to `okilum/<build>/`, and to the fixed
   `okilum/macos/beta/Okilum.dmg`; promotion copies it to `okilum/macos/stable/Okilum.dmg`.

The ZIP is what Sparkle installs; the DMG is only for the first install. To change
the DMG window, edit `scripts/macos-dmg/background.py` and re-render the PNGs.

To release without a new commit, run the workflow by hand on `main`
(Actions → macos-release → Run workflow). It produces the next build number.

The feed is public: `https://updates.befeast.com/okilum/appcast.xml`, with ZIPs at
`https://updates.befeast.com/okilum/<build>/`. The host is shared by BeFeast Mac
apps: Cloudflare R2 bucket `befeast-updates` (custom domain on the `befeast.com`
zone), one folder per app; `release.py --app <name>` picks the folder. `git.oklabs.uk` resolves
only on the home LAN, so it cannot be the feed. Builds up to 5873 still read
`https://git.oklabs.uk/BeFeast/okilum/releases/download/macos-stable/appcast.xml`;
every appcast change is mirrored there, so at home they update onto the public feed.

## Channels and promotion

Every appcast item names a channel: `beta` or `stable`. The app always accepts
`stable`; with **Receive Beta Builds** checked (the default) it also accepts `beta`.

To promote, use the [unified releases workflow](releases.md) with the accepted
macOS build number. It promotes the matching Windows and Arch builds too. The ZIP
and its signature stay the same.

The hand-built 4791/4792 releases accept only Forgejo asset URLs and cannot use the
public feed; install a current ZIP by hand once.

## Isolated Reader profiles

An explicit `OKILUM_STATE_DIR` starts an isolated Reader profile and disables
the native updater (#2723). A copied signed QA bundle still has the production
`CFBundleIdentifier`; Sparkle keys its installer connection by that identity,
not by the Reader profile. Letting both copies update can retain the same helper
and reject the installed app's Restart with “multiple XPC connections”. Use the
normal installed profile for production beta update acceptance. Native updater
fixtures use their own bundle identities and local feeds. Isolated Reader tests
must not acquire the production installation or change its update preferences.

## Rolling back

Sparkle does not downgrade. Each build stays a separate release: download the ZIP
of the build you want from
[Releases](https://git.oklabs.uk/BeFeast/okilum/releases) (or
`https://updates.befeast.com/okilum/<build>/`), replace
`Okilum.app`, and open it. It will offer newer builds again on the next check.

## Keys

- EdDSA private key: Infisical `services/okilum` `SPARKLE_ED_PRIVATE_KEY`, and the
  repository Actions secret of the same name.
- Public key: `SUPublicEDKey`, set from `SPARKLE_PUBLIC_ED_KEY` in
  `macos-release.yml` (also in Infisical as `SPARKLE_ED_PUBLIC_KEY`).
- R2 upload: S3 keys of a Cloudflare API token limited to bucket `befeast-updates`
  (the whole bucket; R2 tokens cannot be limited to a folder),
  in Infisical `services/okilum` (`R2_ACCESS_KEY_ID`, `R2_SECRET_ACCESS_KEY`,
  `R2_ENDPOINT`) and as repository Actions secrets.
- The Developer ID and notary profile live in the runner's login keychain.
  Forgejo vars `OKILUM_SIGNING_IDENTITY` and `OKILUM_NOTARY_PROFILE` select them.

For the operator-only `scripts/open-verified-macos.sh`, supply the expected
`OKILUM_SIGNING_TEAM_ID` and `OKILUM_SIGNING_IDENTITY` from trusted release
configuration. These checks remain mandatory; never infer them from the download.

PR previews are disabled by default (`OKILUM_MACOS_PR_PREVIEWS` must equal `true` to opt in); decision 2026-10-24: test new work in Sprint, Mac beta builds only from main (#1489).
