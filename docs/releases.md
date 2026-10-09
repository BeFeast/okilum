# Releases and promotion

Forgejo builds and publishes each platform's Beta independently. GitHub receives
only public `main`, release tags, and completed cross-platform release artifacts.
No build/signing credentials are needed on GitHub.

## Build cadence and manual builds

Linux builds every merge to `main`. macOS and Windows build the newest `main`
once an hour (the UTC hour boundary), skipping an already-published source.
Mac/Windows have no push trigger: a scheduled/manual run finishes its selected
main snapshot even if new merges arrive. macOS runs daily at 10:30 UTC; Windows runs hourly. No runner sleeps between ticks.
To get an urgent **Mac build now** or **Windows build now**, dispatch the respective
`macos-release` or `windows-release` workflow on `main`; this bypasses the window.
Manual branch builds publish nothing. Windows PR cross-compilation still runs.
Scheduled starts can be delayed by runner queues; the source run number, rather
than the publication time, remains the build version. Rapid merges can supersede
a build before publication; the next scheduled tick or manual build uses newest main.

Release compilation uses pinned sccache with the existing private S3 cache when
credentials are available, otherwise the ordinary compiler. Arch/Windows cache
Cargo downloads and the verified pinned vendor checkout, never per-commit target
archives. Native macOS tests and signing/notarization remain enabled.

## Stable: one accepted source, three builds

CI run numbers differ between platforms. Select the **macOS build number** as the
public release number (`v0.1.<build>`); the release notes list the Windows and Arch
build numbers. All three must name the same source commit. Nothing is rebuilt or
renumbered, so native updater versions remain monotonic.

Each successful publisher writes an artifact descriptor under
`tessera/releases/<source>/<platform>.json`. Windows also archives its portable
ZIP. The coordinator verifies every downloaded asset's size and SHA-256 before
publication. Missing artifacts or mismatched source commits fail closed.

## Rolling Beta

Every successful protected platform publication dispatches **releases** with empty
inputs. A 15-minute reconciliation schedule remains as recovery. The workflow uses
a light runner and no compilation or macOS slot. It reads the current public
Sparkle, Velopack and Arch beta heads and verifies their exact catalog build/source
and asset hashes. Missing catalogs wait; mismatches fail closed. Each platform has
its own rollback guard.

Rolling Beta deliberately contains the latest published build **per platform**;
they need not share a source. Notes list each build and exact SHA. The moving tag
anchors the macOS source, not a claim that Windows/Arch used that commit. Stable
promotion still requires all three platforms to share one accepted source.

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

## Superseded main builds (#562)

All platform builds finish useful work even while main advances. Linux builds on
push; Mac/Windows select main only on their scheduled tick or manual dispatch. Windows
PR checks can still cancel an earlier run of the same PR. Scheduled/manual builds
are not cancelled by a push or the next tick.

Build workflows only upload private Actions artifacts and dispatch `release-publish`.
The publisher requires a successful trusted main run. Linux and scheduled/manual
Mac/Windows snapshots can publish after main advances, but every feed's published
build number is checked under the shared publisher lock. A completion older than
its feed is a no-op. The platform publishers retain their independent rollback
guards. Failed/cancelled builds never publish; legacy Mac/Windows push builds
still require current main.
Versions retain the source build's original
`5000 + run number`; the publication workflow's number is never used.

Forgejo concurrency is workflow-wide. Publication therefore runs in a separate,
serialized workflow with cancellation disabled, sharing the stable-promotion lock.
A main push during an already-started publication lets that short transaction
finish; the newer successful build follows it. Automatic cancellation cannot
interrupt public metadata writes. Existing feed-last/hash/signature checks remain;
this is not a claim of cross-object atomicity under a network failure or manual
cancellation. A publication error is visible in `release-publish`, separately from
the platform build status. Manual recovery can dispatch it with the source run ID
and platform; the same current-main/provenance checks still apply.


### Reserved publication runner

The `publish` label has a dedicated runner process with capacity 1. It must not
also advertise `light`, `ubuntu-latest`, or any PR/build label. Publication,
release mirroring, and scheduled/manual release selection use this slot; Windows
PR selection stays on `light`. Rust compilation stays on the build runners.

Provision this runner before deploying the workflows. Use its own service user,
registration, work directory and container resource slice (1 CPU / 2 GiB per
container; 2304 MiB slice ceiling), independently of the build pool. Disable its
runner cache: publication does not compile. Merely adding a label to an existing
capacity-1 build runner does not reserve a slot. Publication jobs still serialize
with each other and retain their existing feed locks and monotonic guards.

Verify the runner advertises only `publish`, then observe a main mirror and a
release publisher start while a build runner remains occupied. Record queue wait
and merge-to-feed separately; this removes PR queue starvation, not build time.
