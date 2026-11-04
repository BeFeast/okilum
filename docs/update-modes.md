# Update modes (#2467)

Settings → Updates stores one application-wide `update-mode` preference beside
application state, outside vaults and deletable indexes. Missing or legacy values
migrate to Background. Saving is an atomic file replacement; failure leaves the
previous in-memory choice and reports an inline error. Channels remain separate.

| Mode | Startup, activation/focus, 28–32 minute timer | Available release | Install |
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

Linux stages a verified image under the existing installation lock, and never
restages an image retained at Ready, even after another manual check. Cancellation
is observed between payload chunks and before publishing Ready. Dropping the
unarmed stage removes partial bytes and releases the lock; original executable,
launchers and applied-update receipt are untouched. Startup confirmation still
precedes any recovery or update request. The acceptance harness remains isolated.

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
