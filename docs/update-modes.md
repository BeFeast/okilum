# Update modes (#2467)

Settings → Updates stores one application-wide `update-mode` preference beside
application state, outside vaults and deletable indexes. Missing or legacy values
migrate to Background. Saving is an atomic file replacement; failure leaves the
previous in-memory choice and reports an inline error. Channels remain separate.

| Mode | Startup, activation/focus, 28–30 minute timer | Available release | Install |
| --- | --- | --- | --- |
| Background (default) | Check, with the shared five-minute throttle | Download, then Update ready | Explicit Restart now |
| Ask | Same automatic discovery | Version and Download update button | Explicit Restart now |
| Off | No automatic checks, including native Sparkle scheduling | Manual Check shows the offer; Download is explicit | Explicit Restart now |

Manual Check remains immediate in every mode and also postpones automatic
checks. An explicit Download action revalidates the selected channel/feed; it
never installs. A downloaded package is never applied on ordinary Quit/startup without an
earlier explicit Restart click.
Restart keeps editor protection, state persistence, freshness/channel admission,
and window restoration from #2387. Existing channel migration from #2369/#2457
and Linux launcher downgrade protection remain in force.

Linux keeps checking metadata at Ready. An unchanged signed release retains its
verified stage without reacquiring its installation lock or downloading again.
A newer offer discards the superseded unarmed stage, then reacquires the same
installation lock and rechecks the original inode/hash before preparing the new
payload. Conflicting signed identities for the same build remain a visible error.
Cancellation
is observed between payload chunks and before publishing Ready. Dropping the
unarmed stage removes partial bytes and releases the lock; original executable,
launchers and applied-update receipt are untouched. Startup confirmation still
precedes any recovery or update request. The acceptance harness remains isolated.

Windows selects the newest release before admitting a pending package, even when
the previous version is still listed in the feed. Its verification receipt binds
channel, version, the local package digest and the offered full-archive digest.
This preserves a valid SDK delta reconstruction, whose ZIP bytes differ from the
published full archive, without admitting a changed local file, another version
or channel, or a conflicting published identity. Missing/legacy receipts retain
only an original full archive matching the offer; other packages are downloaded
again automatically.

Windows passes cancellation to the existing HTTP UpdateSource. A cancelled
stream removes its partial package, and completion is serialized with cancellation
before Ready is published. If cancellation races SDK validation/rename, the new
pending package and its admission receipt are discarded before releasing the
single-flight guard. A later check starts a fresh transfer. Size/SHA256 checks,
channel revalidation and the explicit restart transaction remain authoritative.

macOS uses the existing silent metadata probe plus custom Sparkle download
path. Native scheduled checks use the same mode and throttle. Sparkle's automatic
installer remains disabled. Download cancellation consumes Sparkle's cancellation
continuation; cancellation at Ready uses Skip, clearing all freshness/session
continuations. Ordinary Quit cancels rather than authorizes installation;
extraction finishes into the same cancellable Ready boundary before shutdown.
If an explicitly clicked Restart has already reached Sparkle installation but
termination was refused, its existing guarded retry/quit transaction is retained;
that case already has installation consent.
Signed freshness checks and explicit-restart editor/recovery guards remain intact.

Cancellation keeps the worker/session busy until cleanup completes. Blocking
network IO observes cancellation at its next chunk boundary (bounded by the
existing request timeout); another check cannot race cleanup. Changing the mode
affects future checks/downloads and does not discard a retained Ready update.

Coverage uses an injected scheduler clock, synthetic release offers, controlled
HTTP/byte streams and real Sparkle with local signed fixture feeds. The native
fixture proves background mode reaches Ready before an automated Restart click,
Ask/Off avoid archive requests, cancellation/ordinary Quit do not install, and
explicit restart retains freshness, editor protection and relaunch behavior.

## Restart feedback and Ready ownership (#2586)

The macOS toast used the existence of Sparkle's retained Ready reply as its
readiness signal. That reply is retained **before** the asynchronous Ready
freshness check finishes. A focus redraw could therefore publish Restart now
while the driver was still verifying. The bridge rejected that click, Rust
ignored the returned boolean, and no retry or visible explanation followed.
A failed freshness/editor check could then discard the reply while leaving the
persistent toast behind; subsequent clicks returned silently too. The tester's
startup-only log cannot identify which of those branches occurred on that Mac.

The toast now waits for verified Ready publication. An explicit click delivered
during verification is retained and resumes through a new signed freshness check
and the final current-editor/state/window-handoff guards. Repeated clicks cannot
consume an additional install continuation. Focus, interval, manual and native
scheduled discovery decline while Ready/retry/preparation/guard ownership is
retained. They cannot replace the downloaded transaction. No click means no
installation; ordinary Quit still cancels the unconsumed Ready reply and waits
for helper cleanup. Existing termination-refusal retries retain their guards.

Every restart attempt and asynchronous outcome is synchronously recorded as
`update_restart` in `reader-diagnostic.log`, the log attached to Feedback.
Sparkle failures are recorded even before a restart attempt. Progress, stale
buttons, busy checks, freshness failure and editor/state refusal have persistent
visible feedback in the application's windows, including a window that lost
focus. A verification taking over 15 seconds revokes that attempt's generation
and reports a retry action; its late callback cannot install. Windows and Linux
use the same feedback/log boundary for busy and failed restarts. Package
verification, channel admission, editor protection and relaunch restoration stay
inside their existing platform transactions.

Hosted native fixtures use real signed archives and the production bridge. New
cases click during initial Ready verification, fire focus/interval/manual rechecks
at verified Ready, repeat clicks during restart verification, and refuse the final
editor guard. A delayed real signed response arrives after the restart deadline;
it must not request the editor guard until a second explicit click starts a new
verification. The pre-fix bridge must reach real download, Ready verification and
the click, then fail to reach the guard/relaunch: compilation/setup failure is not
a passing negative control. A GPUI regression checks persistent refusal feedback
after focus changes and the structured records in the actual Feedback log.

## One update notice, always for the newest update (#2679)

Beta ships on every merge, so several updates can become ready while Okilum is
open. «Update ready» and the restart feedback used to have different notification
ids and stacked: a Mac showed «Update ready · Restart now» under «This update is
no longer ready», the refusal from clicking a notice whose update Sparkle had
already superseded. Some failures also raised a separate error toast.

Every update message now shares one notification id on all three platforms: the
ready notice, restart progress («Verifying…», «Downloading the latest update…»,
«Restarting…») and failures. A newer message replaces the notice in place. Each
window shows at most one update notice. A failure carries one action: Restart now
while an update is ready, otherwise Check for Updates. No separate error toast.

macOS announces each ready package by version. A newer package replaces the
notice even after the user closed the older one; a closed notice does not
return for the same package. Restart now never targets the package the notice
was drawn for. If Sparkle has superseded it, the click's consent carries over
to the newest update: the bridge reports «Downloading the latest update» in the
same notice and restarts once that update has passed freshness verification and
the editor guard. If nothing is downloading, the click starts the download. The
intent ends on cancel, verification timeout, a download or verification failure
(shown in the notice), or when no newer update exists. Windows restarts into
the newest pending package verified for the current channel. Linux stages only
one AppImage at a time.

An empty Sparkle termination-retry callback is actionable waiting, rather than
proof that Restart failed (#2709). GPUI can still be completing an asynchronous
quit at that point. The single notice says that restart is finishing and keeps
later install outcomes active. A callback carrying an actual refusal reason
still reports that reason as a failure; freshness and editor guards are unchanged.

## A newer beta while Ready (#2706)

Linux and Windows continue due/focus discovery while retaining a downloaded
update. The same offer keeps its package and dismissed-notice state; a newer
offer replaces the old stage and the single notice. Background mode downloads
it immediately; Ask exposes the newer offer without installing or downloading.
The installed executable and canonical notes remain untouched during replacement.

Restart now on these platforms first rechecks the latest offer asynchronously.
One explicit click carries through a superseding download, including Ask/Off
mode, then runs the final editor/state guards and normal platform restart.
Repeated clicks share the request. Failure, cancellation or withdrawal revokes
that request; a later Ready event cannot install without a new click. Ordinary
startup and Quit still authorize no installation. Queued replacement progress
cannot overwrite the newest Ready notice or its Restart action.

The explicit non-shipping AppImage acceptance fixture retains its isolated local
trust and never requests a production feed. Production-feed beta acceptance is
recorded separately from that fixture.
