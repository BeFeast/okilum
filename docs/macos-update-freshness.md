# macOS update freshness (#1603)

## Behavior and approved lifecycle

Automatic checks stay enabled. Downloads and installations use Sparkle's standard
user-driven flow. The manager explicitly approved this choice on 2026-10-23:
`SUAllowsAutomaticUpdates = false` prevents a previously persisted automatic-download
preference from taking the silent driver, which bypasses user-driver reply blocks.
No release channel, signing key, publication cadence or Stable feed is changed.

The production `TSUserDriver` in `crates/okilum-shell/src/updater/bridge.m` now:

1. Rechecks the appcast after download/extraction, before presenting Ready.
2. Rechecks again when either Restart button is chosen. Rust then protects all
   editors and persists UI state **after** the asynchronous check, before a
   separate guarded FFI entry point consumes the install continuation.
3. Defers ordinary AppKit termination during download/extraction/verification,
   and rechecks a prepared update on ordinary Quit. This matters even with silent
   automatic installation disabled: Sparkle installs prepared interactive updates
   when the app exits. Other application delegate callbacks and existing
   termination refusal are forwarded unchanged.
4. Cancels a superseded prepared installer using its public
   `SPUUserUpdateChoiceSkip` reply, waits for `didFinishUpdateCycleForUpdateCheck`,
   then starts a new user-initiated Sparkle check. The explicit Install intent
   continues for ordinary updates; informational, major and failed-feed-signature
   prompts are not automatically accepted. Ready-stage Skip cancels without
   recording a user version skip. An installer inherited from an older build
   enters the same hook through the Installing-stage user-driver callback.
5. Cancels installation and reports an error if freshness cannot be established,
   the selected channel changes during verification, the eligible feed regresses,
   or the final editor guard refuses. It never deletes Sparkle caches or changes
   source notes. A normal Quit that encounters a superseded update is refused
   while Sparkle offers/downloads the replacement; it does not silently exit and
   install the old archive.

The freshness probe is a controlled **metadata-only** `SPUUpdater`, not another
installer. It receives a temporary host bundle with a distinct identity, the
running host version and unchanged validation/security configuration. It copies
the effective feed URL/HTTP headers and uses the same allowed channels. Only
`SPUUpdateCheckUpdateInformation` is permitted; scheduling, downloads and
installation are disallowed. Sparkle owns eligibility, OS compatibility, version
selection, appcast validation and archive validation; there is no XML picker or
`bestValidUpdateInAppcast` override. A separate identity is necessary because a
probe on the installation updater can resume its retained N instead of fetching
the appcast. Manual inline checks also use this metadata-only path.

All Sparkle calls, reply blocks, continuation ownership and AppKit termination
replies run on the main thread. Cancellation precedes a new cycle; callbacks from
a dismissed driver are rejected by a generation token. Freshness is bounded to
the last successful appcast fetch, not a guarantee about publication after it.

## Evidence and incident uncertainty

The full rewritten [issue #1603](https://git.oklabs.uk/BeFeast/okilum/issues/1603)
was read through the Forgejo API on 2026-10-23. The reported screenshot shows
15878 offering 15910 when 15910 was current; that establishes discovery at that
moment, not the version actually installed.

The pinned Sparkle 2.10.0 source, tag commit
`eef1a539a373c1f1a320624b1130fc5de7b2e100`, establishes the mechanism:

- [SPUUpdater.m](https://github.com/sparkle-project/Sparkle/blob/2.10.0/Sparkle/SPUUpdater.m)
  resumes an existing installer/download before fetching the feed, including
  information-only probing on that same updater.
- [SPUBasicUpdateDriver.m](https://github.com/sparkle-project/Sparkle/blob/2.10.0/Sparkle/SPUBasicUpdateDriver.m)
  bypasses `shouldProceedWithUpdate` when resuming.
- [SPUAutomaticUpdateDriver.m](https://github.com/sparkle-project/Sparkle/blob/2.10.0/Sparkle/SPUAutomaticUpdateDriver.m)
  bypasses the user driver's Ready reply; its delegate notification cannot cancel
  install-on-quit. This is why the approved user-driven mode is necessary.
- [SPUUserDriver.h](https://github.com/sparkle-project/Sparkle/blob/2.10.0/Sparkle/SPUUserDriver.h)
  defines the supported cancellation reply used by this implementation.

The read-only search of retained evidence under `/home/god/work/qa-evidence` and
`/home/god/.cache/okilum-qa` found no incident client installation trace. The six
October 10 native-log examples in `1155/s/native-formats/macos-log-show.ndjson`
are unrelated. Release logs prove publication, not the client-installed archive.
Oleg's specific root cause remains a hypothesis supported by this source path;
there is no claim of a reproduced incident on his Mac.

## Hosted regression

`scripts/ci/sparkle-freshness/run.py` builds an isolated AppKit host including the
**production bridge**, and uses the pinned, unpatched Sparkle framework. The
fixture creates ephemeral Ed25519 keys with CryptoKit, signs actual update ZIPs
and appcasts with the official `sign_update`, and updates throwaway app bundles.
No production credentials, test-feed override in the product, validation bypass,
M4 access or local test execution is needed.

The host automates standard UI choices, not version selection or Sparkle state.
After actual N=2 download/extraction it publishes a signed feed containing N+2=4.
The test asserts the real callback ran, N was rejected at readiness, the new
archive was downloaded, and the installed bundle plus relaunch receipt are 4.
An unchanged-feed control must install 2 with the same hook. Additional cases
cover a feed changing while Ready is displayed, ordinary Quit, Quit during
extraction, stable-only selection, a rejected archive signature, an older eligible
feed, malformed feed, and refusal at the final editor-guard boundary. The latter
exercises the production bridge's refusal/cancellation continuation; it does not
claim to simulate an actual Rust editor save failure.

The normal GitHub-hosted macOS PR lane invokes this fixture from
`scripts/ci/check-macos.sh` and uploads logs, callback traces and feed requests.
Corporate/local runners do not execute it. These tests require a hosted CI result
before merge; writing the test is not evidence that it passed.

## Beta cadence recommendation (proposal only)

Recommend latest-wins coalescing of Mac beta publication to at most one per
30 minutes during the every-merge window, ending 2026-10-26 06:00 Israel
(`2026-10-26T04:00:00Z`, after the daylight-saving transition). An explicit manual
urgent bypass should record the tester-blocking reason. Coalesce pending heads
before building; do not interrupt signed publication or publish older heads later.

This reduces redundant downloads/relaunches and signing-runner pressure, at the
cost of up to 30 minutes of ordinary delivery latency. It does not replace the
freshness fix. No cadence/workflow policy change is implemented here; manager
direction is still required for that separate proposal.
