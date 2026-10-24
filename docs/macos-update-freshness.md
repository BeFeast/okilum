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
   separate guarded FFI entry point consumes the install continuation. Ordinary
   Quit uses this same post-verification Rust protection/persistence boundary;
   only after it succeeds can AppKit accept termination without relaunching.
3. Cancels ordinary AppKit termination during download/extraction/verification,
   and rechecks a prepared update on ordinary Quit. This matters even with silent
   automatic installation disabled: Sparkle installs prepared interactive updates
   when the app exits. Other application delegate callbacks and existing
   termination refusal are forwarded unchanged.
4. Cancels a superseded prepared installer using its public
   `SPUUserUpdateChoiceSkip` reply, waits for `didFinishUpdateCycleForUpdateCheck`,
   then starts a new Sparkle check synchronously inside that completion callback,
   preserving foreground/background context. The explicit Install intent
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
installer. Its temporary bundle has a distinct installation identity but uses
Sparkle's supported `SUDefaultsDomain` to share the source's actual selection
preferences, including skips and phased rollout group. This includes the first
assignment of a rollout group; no private preference-key copying or independent
random group is involved. The current cycle's `state.userInitiated` determines
the check type: foreground `checkForUpdates` with a metadata driver that always
replies Dismiss, or background `checkForUpdateInformation`. Replacement cycles
keep the same foreground/background context. Manual inline probes retain their
normal background skip/rollout semantics.

The probe copies the source version/security configuration, effective feed URL,
HTTP headers and allowed channels. A framework symlink preserves the actual
Sparkle executable/locale inputs to delta eligibility. Sparkle owns OS/channel,
skip/phase, version and delta selection and signature validation. There is no XML
picker or `bestValidUpdateInAppcast` override. Only the requested check type is
permitted, the metadata driver never accepts a download, and the transient
updater is released on completion so it retains no second scheduler. A separate
installation identity is necessary because probing the installation updater can
resume retained N instead of fetching the feed.

All Sparkle calls, reply blocks, continuation ownership and AppKit termination
requests run on the main thread. Cancellation precedes a new cycle; callbacks from
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

## Hosted replacement stall after #1605

The retained hosted run `40082795045` reached `prepared 2`, published the changed
feed, fetched the appcast again, and reached `dismiss 2`. The installed bundle
remained 1 with no relaunch receipt. This excludes absence of the GUI/download
callbacks; it does not retrospectively verify the original client incident.

Pinned Sparkle 2.10.0 `SPUUpdater.m` explains the deterministic scheduler conflict:

- Its driver completion clears `_driver` and `sessionInProgress`, then calls
  `didFinishUpdateCycleForUpdateCheck:error:` (lines 868–888).
- After that delegate returns, it schedules the next automatic check unless the
  delegate has already started a session. Interval discovery immediately sets
  `sessionInProgress = YES` before its asynchronous installer-info probe
  (lines 502–549).
- Both public foreground and background check methods return without checking
  when that flag is already set (lines 664–667 and 713–716).

The bridge previously dispatched the replacement check onto the main queue from
cycle completion. That dispatch ran after scheduling had reserved the session,
so Sparkle ignored it. Start the replacement directly in the public completion
callback instead: Sparkle has already finished driver cleanup, and its explicit
post-delegate session check then suppresses the competing scheduler. Cancellation
still uses Ready-stage Skip; no restart is initiated from its reply block.
Automatic checks remain enabled. No timer delay or timeout increase is needed.

The hosted delegate asserts that replacement has claimed a session before cycle
completion returns, and traces that boundary. Existing signed archive, actual
N→N+2 installation/relaunch, unchanged-feed, foreground/background eligibility
and post-verification editor-guard cases remain mandatory. Static source evidence
establishes the conflict; hosted CI on the fix head is still required to establish
successful installation and detect any subsequent lifecycle failure.

### Ordinary Quit modal-loop stall

Hosted run `40084047136` on `1ed6e809` passed `newest`, `background-newest`,
`background-phase-unchanged` and `restart-newest`, then timed out in `quit-newest`.
The Quit trace reached `verified-ready 2`, published N+2, and reported verification
starting, but made no subsequent appcast request and never reached the editor
guard. Evidence is retained under `~/work/qa-evidence/1634/quit-newest/`.
The installed-bundle snapshot was taken after the fixture terminated the timed-out
process; its version 2 is not evidence that installation preceded the timeout.

The fixture requests ordinary Quit from the real Sparkle callback on the main
queue. Our delegate returned `NSTerminateLater` while requesting asynchronous
freshness. Apple's [termination contract](https://developer.apple.com/documentation/appkit/nsapplication/terminatereply/terminatelater)
states that this runs a nested `NSModalPanelRunLoopMode` loop until the app replies.
It cannot drain pending blocks from the main queue whose callback is still inside
`terminate:`. Verification and the default-mode editor-guard timer therefore
cannot complete; this is independent of the replacement scheduling fix above.

Return `NSTerminateCancel` for the first Quit instead, so the initiating callback
can unwind. After fresh verification and the Rust editor guard succeed, reissue
ordinary Quit with a one-use approval. The original application delegate still
makes the final decision, including refusal or its own deferred reply. Guard
refusal and supersession keep the app open without arming that approval. This
does not use Sparkle's Restart/relaunch continuation. Hosted Quit cases assert
that the first Quit returns, the guard precedes the original delegate's decision,
and the real N+2 installation occurs without a relaunch receipt.

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
feed, malformed feed, and post-verification refusal for both Restart and ordinary
Quit. The host links the same Rust final-guard coordinator as production and
changes a file-backed editor after verification; accepting Quit/Restart must
protect that new revision, while refusal must leave the app installed at 1.
A separate hosted GPUI regression calls the production protection binding with
an actual Reader editor: it verifies the latest edit is saved before commit and
that a real recovery-directory failure invokes refusal instead of commit.

Eligibility fixtures compare the actual source updater selection and the
production probe's version and payload URL under foreground/background phased,
minor-skip and major-skip contexts, a custom defaults domain, OS filtering and
delta selection. They assert metadata checks download no archive. Foreground
phased unchanged-feed and background phased unchanged-install controls ensure
that valid N is neither rejected nor replaced by an ineligible newer version.

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
