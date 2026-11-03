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
