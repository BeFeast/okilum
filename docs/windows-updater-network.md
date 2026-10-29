# Windows updater system network policy (#1678)

Velopack1.2.161 HttpSource accepts headers/timeouts but no custom HTTP agent or
Windows trust/proxy resolver. Its supported UpdateSource interface can replace
only feed/package transport. Implement that interface with okilum-http's Windows
SChannel trust and current-user static/PAC/WPAD routing, preserving explicit
proxy-environment precedence and bypass rules. No certificate/hostname bypass,
implicit Windows credentials or alternate endpoint is added.

Stable/beta selection and URLs remain in windows_feed. Preserve localVersion,
id and stagingId query values with URL encoding, send an explicit Okilum version
User-Agent, require HTTPS feed bases and disable redirects. Plain HTTP loopback
is constructed only inside cfg(test), never via the production constructor. A feed is bounded to4MiB; package filenames
must be safe ASCII basenames. Stream downloads under the declared size and retain
progress. Velopack still owns its update lock, delta fallback, checksum/size
validation, partial→final rename, apply and restart. The optional public-utils
feature exposes the Manifest type required by the public UpdateSource signature;
it does not disable verification or fork Velopack.

The inline updater status shows the short first error line. The existing bounded
app diagnostic sink receives the complete chain, including source transport
context; checksum/install errors retain their own reason. Integrated NTLM/Kerberos
authentication remains unsupported and a407 is explained.

## Validation contract

Loopback tests exercise feed/query/User-Agent, actual byte streaming/progress,
size mismatch, HTTP/redirect rejection, metadata cap and unsafe-path rejection.
An actual UpdateManager regression must reject a wrong SHA256 without publishing
the final package and accept a correct hash before its rename.

The existing Windows native dispatch with package=okilum-http, features=updates
also executes isolated native controls. Its fixture never forwards and exposes
only GET metadata/package and PAC. An untrusted CA must fail; importing a temporary
LocalMachine Root on the ephemeral hosted runner must still fail the old ureq/webpki policy but allow production
Source with system static proxy and PAC. Receipts retain source and request/UA
observations. The CA import and each child process have explicit deadlines. Root/proxy/process cleanup runs in finally; private fixture keys are
never uploaded. This is native transport/download evidence, not a claim that a
GUI installer or Restart now was tested by the companion.

Rule7a native installation/update acceptance remains required before merge. Use
an exact-source QA package in an isolated harness slot, preserve real checksum,
install/restart and data checks, and distinguish unavailable acceptance from a
product failure. The Feedback-only merge exception does not apply to this PR.
No signing, promotion or public release change is authorized by this document.
