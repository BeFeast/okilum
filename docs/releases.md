# Releases and promotion

Forgejo builds and publishes each platform's Beta independently. GitHub receives
only public `main`, release tags, and completed cross-platform release artifacts.
No build/signing credentials are needed on GitHub.

## One accepted source, three builds

CI run numbers differ between platforms. Select the **macOS build number** as the
public release number (`v0.1.<build>`); the release notes list the Windows and Arch
build numbers. All three must name the same source commit. Nothing is rebuilt or
renumbered, so native updater versions remain monotonic.

Each successful publisher writes an artifact descriptor under
`tessera/releases/<source>/<platform>.json`. Windows also archives its portable
ZIP. The coordinator verifies every downloaded asset's size and SHA-256 before
publication. Missing artifacts or mismatched source commits fail closed.

## Rolling Beta

The Forgejo **releases** workflow checks every 15 minutes for a complete set, using
a light runner and no compilation or macOS slot. An empty manual dispatch also
refreshes Beta. Incomplete source sets wait for the remaining platform; individual
platform update feeds keep working independently.

The GitHub prerelease named **Beta** is updated in place, with one moving `beta`
tag. It contains the macOS ZIP, Windows installer and portable ZIP, Arch package
and signature, and `SHA256SUMS`. During asset replacement it is temporarily a draft,
so partially replaced assets are not offered as a completed release.

## Stable promotion — explicit approval required

The first stable promotion is gated on owner QA: all OSes must open as quickly as
macOS. Implementing or merging this workflow does **not** authorize promotion.

After approval, run **releases** on **main** with `build` set to the accepted macOS
Beta build. This is the single supported promotion action for all OSes. The old
platform-specific manual promotion inputs are removed.

The action validates all artifacts and Arch signatures, mirrors the release tag
with public main, then promotes the signed Arch repository, Windows stable feed,
and macOS appcast. It publishes GitHub **Tessera 0.1.<build>**, marked Latest, with
the original files and merged PR titles since the previous stable source.

Stable URLs stay fixed:
- `https://updates.befeast.com/tessera/macos/latest.zip`
- `https://updates.befeast.com/tessera/windows/stable/Setup.exe`
- [Arch stable repository](linux-releases.md)

Channels on separate systems cannot change atomically. A saved promotion manifest
pins the exact source and platform builds. If a network/API failure interrupts
publication, rerun the **same build**; do not select a different one until it
finishes. If that build is irrecoverable, the owner may select a **newer** build
and explicitly enable `supersede_pending`. All artifact and channel rollback
checks still run before replacing the saved selection; this never rolls back a
partially promoted channel. The final `tessera/releases/stable.json` is written only after all feeds
and GitHub assets succeed. Completed retries are no-ops; rollback is refused.

The `github-mirror` job only pushes `main` and the selected reachable `v0.1.*` or
`beta` tag. It never mirrors other branches, internal tags, or archived history.
Secrets are the existing R2, Arch signing and `TESSERA_GITHUB_MIRROR` credentials;
`FORGEJO_TOKEN` comes from the Actions token. Stable promotion shares the macOS
appcast publication lock; scheduled Beta does not block the macOS release queue.
