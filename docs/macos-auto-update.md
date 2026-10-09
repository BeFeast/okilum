# macOS releases and updates

Okilum updates itself with stock [Sparkle 2](https://sparkle-project.org) (2.10.0,
pinned by `scripts/updater/sparkle-lock.json`). The app uses
`SPUStandardUpdaterController` and Sparkle's own UI. The application menu has
**About Okilum**, **Check for Updates…** and **Receive Beta Builds**. Sparkle also
checks every hour on its own.

## How a build is released

The daily 10:30 UTC newest-main build (or manual build now) runs [`macos-release.yml`](../.forgejo/workflows/macos-release.yml)
on the existing M4 runner (label `macos`):

1. build `Okilum.app` with `CFBundleVersion = 5000 + run number` and
   `CFBundleShortVersionString = 0.1.<build>` (shown in About);
2. sign with the Developer ID, notarize, staple, ZIP, check Gatekeeper on a
   quarantined copy (`scripts/build-macos-ci.sh`, `scripts/updater/sign-bundle.sh`);
3. sign the ZIP with the EdDSA key (`sign_update`);
4. upload the ZIP to the public feed, add the build to its `beta` channel, and keep
   a copy as Forgejo release `macos-stable-<build>` (`scripts/updater/release.py`).

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
