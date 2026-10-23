# macOS update freshness: #1603 investigation

Status: blocked on a lifecycle change; this document does not fix installation.
Read against main `6e39977e44a0072ccd754147249f3f75becc28f5` and the full
rewritten [issue #1603](https://git.oklabs.uk/BeFeast/okilum/issues/1603),
fetched anonymously through the Forgejo API on 2026-10-23.

## Evidence and uncertainty

The issue reports repeated beta update/relaunch steps. Its screenshot shows
15878 offering 15910 when 15910 was the newest appcast entry. That supports
correct discovery at that moment; it does not identify the installed archive.

`crates/okilum-shell/src/updater/bridge.m` uses the stock
`SPUStandardUpdaterController`. Install calls `checkForUpdates:` directly.
The bridge has no freshness check, cancellation handler or install-version log.
The reporter receives `displayVersionString` from `didFindValidUpdate`, not an
installation receipt. No custom oldest-first selection exists in this bridge.

A read-only search of retained files under `/home/god/work/qa-evidence` and
`/home/god/.cache/okilum-qa` found no incident Sparkle installation trace.
The retained `1155/s/native-formats/macos-log-show.ndjson` contains six unrelated
October 10 native-log examples, not this update session. Release producer and
publisher logs establish publication, not which archive the client installed.
This is missing evidence, not a negative reproduction with a positive control.
No M4 access, native execution or host changes were used.

## Verified pinned Sparkle behavior

The lock pins Sparkle 2.10.0. Source inspected at upstream tag commit
`eef1a539a373c1f1a320624b1130fc5de7b2e100`:

- [SPUUpdater.h](https://github.com/sparkle-project/Sparkle/blob/2.10.0/Sparkle/SPUUpdater.h):
  `checkForUpdates` explicitly may present an already downloaded/installing update.
- [SPUUpdater.m](https://github.com/sparkle-project/Sparkle/blob/2.10.0/Sparkle/SPUUpdater.m):
  `checkForUpdatesWithDriver:updateCheck:installerInProgress:` resumes the installer,
  then a retained resumable update, and only otherwise checks the feed. Probing
  also reaches this routine; it is not a guaranteed independent fresh feed fetch.
- [SPUBasicUpdateDriver.m](https://github.com/sparkle-project/Sparkle/blob/2.10.0/Sparkle/SPUBasicUpdateDriver.m):
  `notifyFoundValidUpdateWithAppcastItem:...resuming:` calls
  `shouldProceedWithUpdate:updateCheck:error:` only when `resuming` is false.
  Adding this delegate cannot veto an already downloaded N.
- [SPUUpdaterDelegate.h](https://github.com/sparkle-project/Sparkle/blob/2.10.0/Sparkle/SPUUpdaterDelegate.h):
  `willInstallUpdateOnQuit:immediateInstallationBlock:` says **in either case**
  Sparkle attempts installation on termination. Returning YES stalls the cycle;
  returning NO does not cancel. `shouldPostponeRelaunchForUpdate:` delays relaunch,
  not a supported discard-and-select-newer operation. `willInstallUpdate:` is void.
- `resetUpdateCycle` reschedules checks; it does not clear the retained update.
  `automaticallyDownloadsUpdates = NO` prevents future automatic downloads,
  but is not a discard API for an already downloaded/installing archive.

Thus the reported mechanism is supported by the actual integration and pinned
source, but the cause of Oleg's particular incident remains unverified.
A fresh version-choice helper followed by the existing install call would not
satisfy the issue: Sparkle could still resume N.

## Smallest reviewable alternative

Keep the vendor unmodified. Review a narrowly scoped application-owned
`SPUUserDriver` adapter around the standard user driver, rather than adding a
policy helper to the existing delegate. The public
[SPUUserDriver.h](https://github.com/sparkle-project/Sparkle/blob/2.10.0/Sparkle/SPUUserDriver.h)
provides reply blocks at the real download/install boundaries:

- At `showUpdateFoundWithAppcastItem:state:reply:`, Skip clears a resumed
  downloaded update through Sparkle's own lifecycle, but also records a skipped
  version. Do not silently turn supersession into the user's skip preference.
- At `showReadyToInstallAndRelaunch:`, Skip cancels an already installing update
  without skipping that version in the future. This is distinct from Dismiss,
  which may still install on exit.

An adapter still needs an independent, channel-aware Sparkle selection path,
serialized cancellation completion before restarting a check, and interception
before install-on-quit is armed. The existing standard-controller delegate
cannot supply those guarantees. Establish them in an approved lifecycle design
before implementing the adapter; do not introduce a second uncontrolled updater,
raw XML selection, private selectors, cache deletion, installer termination or
vendored source patches. A fallback that only disables future beta predownloads
reduces exposure but does not meet the requested downloaded-N regression.

Preserve stable behavior, allowed-channel filtering, monotonic version comparison,
OS compatibility, appcast/archive validation, signatures and all user data. On
failed freshness verification, retain the running app and surface the failure;
do not claim N is current. Bound freshness to the last successfully fetched
appcast: publication after that instant cannot be promised away.

## Required regression before claiming a fix

Hosted macOS integration must exercise the production adapter and real Sparkle
lifecycle against an isolated signed test feed: run an older app, download N,
publish N+2, invoke the real Install action, and verify the installed bundle is
N+2 after one relaunch. Cover automatic install-on-quit separately. Observe feed
requests and cancellation completion, then assert N was not installed. A control
with unchanged appcast must successfully install N using the same harness, so
an untriggered hook cannot pass as prevention. Also cover unavailable feed,
signature rejection, stable-only filtering and an older appcast candidate.
A mock version-choice test alone is insufficient. These regressions have not
been implemented or run by this documentation change.

## Temporary beta cadence recommendation (not implemented)

Recommend latest-wins coalescing of Mac beta publication to at most one per
30 minutes during the every-merge window, ending 2026-10-26 06:00 Israel
(`2026-10-26T04:00:00Z`, after the daylight-saving transition). Keep an explicit
manual urgent bypass for a tester-blocking fix, with a recorded reason; it is an
exception to the cap. Coalesce pending source heads before starting a build;
do not interrupt an active signed publication or publish older heads afterward.

This reduces redundant downloads/relaunch prompts and signing-runner pressure,
but delays ordinary delivery by up to 30 minutes and does not correct stale
installation. Validate that the current head's normal gates passed before
publication. Stable and other platforms remain outside the proposal. Manager
direction is required before any cadence/workflow change.
