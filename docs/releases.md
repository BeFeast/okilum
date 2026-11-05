# Releases and promotion

Forgejo builds and publishes each platform's Beta independently. GitHub receives
only public `main`, release tags, and completed cross-platform release artifacts.
No build/signing credentials are needed on GitHub.

## Build cadence and manual builds

Forgejo workflows do not run on pull requests, and main has no required status
checks. Before merge, each worker runs `H=github scripts/agent/gh-check.sh` on
its committed exact HEAD, waits for green, then merges the PR themselves.
A later commit requires a fresh check. See `docs/agent-checks.md`.

Forgejo check, full shell, source-sharing and acceptance suites (`fast`, `ci`,
`brain-ci`, `inbox-ci`, compatibility) run on nightly schedules and manual
dispatches, never on main pushes; only beta packaging and the GitHub mirror run on
push. Legacy native-check ref cleanup is manual maintenance.

Before Stable promotion, require a successful `fast / shell-full` on the exact
source being promoted, including its `(workflow_dispatch)` certification status.
Never substitute a newer main result or a PR result for that source. If absent,
dispatch `fast.yml` on main with `certification_source` set to the candidate SHA
and wait for that source's full-suite result. Certification is on demand only:
nothing certifies main automatically any more, and Beta never reads it.

Since #2589 both certification jobs wait on a `bridge` runner while GitHub's
dedicated `ubuntu-24.04` runners execute that source's own commands: `check`
runs the CI tooling, releases and updater Python suites and
`scripts/ci/fast.sh` (`.github/workflows/forgejo-check.yml`, two parallel jobs
of one run), `shell-full` runs `scripts/ci/shell-full.sh`
(`.github/workflows/forgejo-shell-full.yml`). The status contexts Stable reads, `fast / check (workflow_dispatch)` and
`fast / shell-full (workflow_dispatch)`, and their reporting jobs are unchanged.
GitHub being unavailable is a failure, never a fallback pass. A source that
predates a hosted workflow fails at once with `predates hosted check` (or
`predates hosted shell-full`); certify a newer main commit. Nightly and
ordinary manual runs use the same hosted jobs. Details and budgets:
`docs/fast-runner.md`.

### Beta on every main push (Oleg 05.11)

Every push to `main` starts `linux-release`, `macos-release` and
`windows-release` for **that exact commit**. There is no cron and no
certification gate: merges are gated by the worker's exact-HEAD gh-check, and
each platform's compile, package and signing run is the beta gate. The only
skip is a pushed commit that already is that platform's beta (the actual feed
head, not an archive catalog).

A newer push cancels the unfinished beta of an older commit, per platform:
push runs share the concurrency group `<platform>-beta-main` with
`cancel-in-progress`. Forgejo cancels every run of that group that still has a
waiting, blocked or running job. The hosted GitHub producers follow: Linux
AppImage producers share the GitHub group `forgejo-linux-beta`, and Mac beta
signing runs share `macos-shipping-beta`, both cancelling the older run
(the Mac keychain cleanup is `always()` and still runs). Manual, pinned,
archive, AppImage-only and Windows re-sign dispatches keep their own Forgejo
groups: a push never cancels their Forgejo run, and they never cancel a push
run. A manual main beta's hosted Linux/Mac producer is still superseded by a
newer push's producer, which is the intended outcome. Trade-off: merges that
arrive faster than a platform's build time keep restarting that platform's beta
until main is quiet for one build.

Linux retains exact run/job/artifact SHA provenance by creating a write-once
GitHub branch `forgejo-linux-pr/hourly-<source>` (the name predates push betas).
Its push starts the producer; existing completed or active producers are reused.
A mismatched existing ref is an error, never force-updated. The branch is
created only after the GitHub mirror has the pushed commit (bounded wait, 10
minutes); the Mac bridge waits the same bound for the mirror. Mac and Windows
build the pushed product source with their existing delivery tooling.

Push runs carry Forgejo's native statuses on their own commit, named
`<platform>-release / <job> (push)`; the 09:00 Stable selector matches them.
Manual dispatches report the same contexts with `(workflow_dispatch)` through
`shipping-status`. Signing, channels and Stable promotion are unchanged;
Windows push betas stay unsigned.

Manual branch builds publish nothing. Explicit older-source archive and Windows
re-sign requests retain their existing routes. A manual main dispatch (recovery
after a failed push build, or an urgent rebuild) builds the newest unpublished
first-parent main; a dispatch whose snapshot is older than main re-dispatches
itself pinned to main's tip.

To observe it after a merge: within a minute of the push, Forgejo Actions shows
three new `push` runs on that commit (`linux-release`, `macos-release`,
`windows-release`) plus `github-mirror`. Linux creates
`forgejo-linux-pr/hourly-<sha>` on GitHub and a `forgejo-linux` run there; Mac
starts a `forgejo-macos-shipping` run. Each successful build dispatches
`release-publish`, which then refreshes `releases` (the GitHub rolling Beta).
A second merge before those finish shows the first commit's runs as cancelled.

Release compilation uses pinned sccache with the existing private S3 cache when
credentials are available, otherwise the ordinary compiler. Arch/Windows cache
Cargo downloads and the verified pinned vendor checkout, never per-commit target
archives. Native macOS tests and signing/notarization remain enabled.

## All-platform Stable: one accepted source, three builds

CI run numbers differ between platforms. Select the **macOS build number** as the
public release number (`v0.1.<build>`); the release notes list the Windows and Arch
build numbers. For an all-platform promotion, all three must name the same source
commit. Linux-only Stable can advance independently; Stable sources may differ by
platform. Nothing is rebuilt or renumbered, so native updater versions remain
monotonic.

Each successful publisher writes an artifact descriptor under
`okilum/releases/<source>/<platform>.json`. Windows also archives its portable
ZIP. The coordinator verifies every downloaded asset's size and SHA-256 before
publication. Missing artifacts or mismatched source commits fail closed.

## Rolling Beta

Every successful protected platform publication dispatches **releases** with empty
inputs. There is no polling schedule; if that refresh fails, the next publication
or a manual `releases` dispatch with empty inputs reconciles it. The workflow uses
a light runner and no compilation or macOS slot. It reads the current public
Sparkle, Velopack and Arch beta heads and verifies their exact catalog build/source
and asset hashes. Missing catalogs wait; mismatches fail closed. Each platform has
its own rollback guard.

Rolling Beta deliberately contains the latest published build **per platform**;
they need not share a source. Notes list each build and exact SHA. The moving tag
anchors the macOS source, not a claim that Windows/Arch used that commit.
All-platform Stable promotion still requires all three platforms to share one accepted source;
Linux-only promotion uses its own accepted source/build.

The GitHub prerelease named **Beta** is updated in place, with one moving `beta`
tag. It contains the macOS ZIP, Windows installer and portable ZIP, Arch package
and signature, and `SHA256SUMS`. During asset replacement it is temporarily a draft,
so partially replaced assets are not offered as a completed release.

## Stable promotion

The working process in [AGENTS.md](../AGENTS.md) governs Stable selection and
promotion. Manager approval is not a release gate. The coordinator's artifact,
signature, CI and recovery checks still apply; Windows requires Oleg's signing.

For the all-platform coordinator path, run **releases** on **main** with `build`
set to the accepted macOS Beta build. This promotes all OSes together; the explicit
Linux-only path below is independent. Use these protected coordinator paths for promotion.

The action validates all artifacts and Arch signatures, mirrors the release tag
with public main, then promotes the signed Arch repository, Windows stable feed,
and macOS appcast. It publishes GitHub **Okilum 0.1.<build>**, marked Latest, with
the original files and merged PR titles since the previous stable source.

Stable URLs stay fixed:
- `https://updates.befeast.com/okilum/macos/latest.zip`
- `https://updates.befeast.com/okilum/windows/stable/Setup.exe`
- [Signed Linux AppImage](https://updates.befeast.com/okilum/linux/stable/Okilum-x86_64.AppImage) · [signature verification and legacy Arch migration](linux-releases.md)

Channels on separate systems cannot change atomically. A saved promotion manifest
pins the exact source and platform builds. If a network/API failure interrupts
publication, rerun the **same build**; do not select a different one until it
finishes. If that build is irrecoverable, the owner may select a **newer** build
and explicitly enable `supersede_pending`. All artifact and channel rollback
checks still run before replacing the saved selection; this never rolls back a
partially promoted channel. The final `okilum/releases/stable.json` is written only after all feeds
and GitHub assets succeed. Completed retries are no-ops; rollback is refused.

The `github-mirror` job only pushes `main` and the selected reachable `v0.1.*` or
`beta` tag. It never mirrors other branches, internal tags, or archived history.
Secrets are the existing R2, Arch signing and `OKILUM_GITHUB_MIRROR` credentials;
`FORGEJO_TOKEN` comes from the Actions token. Stable promotion shares the macOS
appcast publication lock; scheduled Beta does not block the macOS release queue.

### Linux-only Stable (#1432)

Dispatch **releases** on **main** with both the exact Linux
`build` and full lowercase `source` SHA, and `linux_only=true`. Leave
`windows_only`, `skip_windows`, `supersede_pending` and `linux_rollback` false.
For the first accepted pilot the inputs are:

```json
{"ref":"main","inputs":{"linux_only":"true","build":"14108","source":"f4b64e77f93448c6204c806204873528a699f355","linux_rollback":"false","windows_only":"false","skip_windows":"false","supersede_pending":"false"}}
```

The workflow invokes `scripts/releases/linux.py --build 14108 --source <full SHA>`
in the existing protected publisher, under `okilum-macos-release` with cancellation
disabled. Operators must use this protected workflow; do not invoke an Arch or
Linux publication CLI outside it or bypass the shared lock. It does not request
or compile any build. It verifies main ancestry, exact Linux catalog/archive source and build, expected asset paths, sizes and
checksums, package identity, and package/repository signatures from the configured
full signing fingerprint. No macOS or Windows artifact is required or accessed.
It writes only Arch stable payloads/feeds and Arch promotion receipts. The shared
public signing key, Beta, all-platform `releases/stable.json`, GitHub release and
release tags remain unchanged. Windows signed-only enforcement remains unchanged.
Linux Stable's authoritative head is `arch/stable/x86_64/latest.json`.

The publisher uses the existing Arch channel helper to prepare the signed DB before
any remote write. A durable receipt at
`okilum/arch/promotions/<build>-<source>.json` saves the exact before/after feed bytes,
identity, state and timestamps. `okilum/arch/stable/promotion.json` points at the active
receipt. Keep these operational records. Immutable package/signature bytes precede
feed writes; each object is read back before completion. Retry an interrupted
promotion with **identical inputs**. The saved signed DB is reused. A different
selection cannot replace an active interrupted transaction. Ordinary backwards
promotion and conflicting immutable payloads are refused.

For explicitly approved rollback, dispatch the same Linux-only source/build inputs
with **`linux_rollback=true`**. This restores only that receipt's exact previous DB,
signature and head (or removes them if none existed), retaining package downloads.
Rollback does not downgrade installed clients. Retry an interrupted rollback with
the same rollback inputs. Recovery refuses feed bytes that differ from both saved
states, so it cannot overwrite a later publication. After completed rollback that
selection cannot be promoted again; select another accepted build.

**Remaining atomicity gap:** every R2 PUT is atomic for one object, but the Arch DB
and detached signature are two objects. A client crossing their replacement can
fail verification until retry; `latest.json` is written last but pacman does not
use it as a generation pointer. The current consumer cannot provide atomic
replacement of the pair. This path retains `DatabaseRequired` fail-closed behavior
and makes interruption recoverable; it does not claim multiobject atomicity or
change the consumer/infrastructure. A strict requirement for atomic DB/signature
visibility remains unmet by the existing protocol.

### Promoting an older main commit (#1040)

Main often moves on before a source has all three builds: a newer push cancels
an unfinished beta, so a commit that was main's head only briefly may lack one or
more platform builds. To promote such a commit, run **releases** on **main** with `source`
set to its full SHA and `build` empty:

1. The commit must be on main's history and must already have a macOS build. That
   lane signs on the M4 under separate rules; without it the run stops and says so.
2. Linux and Windows builds that are missing for that commit are started with the
   same `source` input on their release workflows. They check out exactly that
   commit, record it in their publication artifact, and are published to the **build
   archive only**: the catalog, `arch/builds/<n>` and `windows/builds/<n>`. The beta
   feeds keep their newer builds.
3. A promotion request is saved in `okilum/releases/requests/<sha>.json`. When the
   last missing build is published, publication starts the ordinary
   `releases build=<mac>` promotion. If nothing was missing, that happens at once.

No step waits on a runner, so publication and promotion keep their single lock. You
can also run the Linux or Windows release workflow by hand with `source` to fill the
catalog without promoting. A commit that already has its build for that platform is
not built again.

## Superseded main builds (#562)

A newer main push cancels the unfinished push build of an older commit (see
"Beta on every main push" above). Manual, pinned and archive builds are not
cancelled by a push.

Build workflows only upload private Actions artifacts and dispatch `release-publish`.
The publisher requires a successful trusted main run. Push and manual snapshots
can publish after main advances, but every feed's published build number is
checked under the shared publisher lock. A completion older than its feed is a
no-op. The platform publishers retain their independent rollback guards.
Failed/cancelled builds never publish.
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

## Independent signed Windows Stable (#1688)

With a signed Windows build selected under v3 and exact-build
installation/feed/Restart/data checks satisfied, dispatch `releases.yml` on `main` with
`windows_only: true`, `build: "<Windows build>"`, and `source: "<full main SHA>"`.
Forgejo's dispatch API represents boolean inputs as strings (`"true"`). This selects
that Windows build, independently of the current Mac Stable build/source. Without
`source`, `windows_only` retains the legacy matching-Mac-build behavior.

The independent route verifies main ancestry, archived source/build, catalog sizes
and hashes, `signed=true` with signer fingerprint, Velopack package checksums and
monotonic Stable version before any write. It requires no compilation or signing.
`OKILUM_WINDOWS_STABLE_REQUIRE_SIGNED` remains `1`; the independent route also
rejects unsigned metadata if that variable is missing. Only Windows Stable package,
Setup.exe and feed plus an exact-identity promotion receipt change. Mac/Linux feeds,
tags, shared catalog Stable marker and notes remain unchanged. Failed publication
can retry the same pinned identity; a different metadata identity fails closed.
