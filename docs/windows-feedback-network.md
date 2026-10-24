# Windows Feedback transport (#1667)

Feedback uses `okilum-http` for client policy while retaining its multipart
payload, timeouts, no-redirect policy and public endpoint. Windows explicitly
selects reqwest's SChannel/native-TLS backend, which validates against the OS
certificate store. Bundled rustls roots remain the policy on other platforms.
No certificate or hostname verification is disabled.

Windows proxy discovery uses the current user's Internet settings and resolves
PAC/WPAD through WinHTTP for the request URL. Explicit proxy environment variables
retain their previous precedence; system bypass rules and NO_PROXY are honored.
PAC fetching does not automatically authenticate with Windows credentials. An
explicit PAC resolution failure stops with an error rather than silently bypassing
corporate routing; unavailable WPAD may use the configured static/direct policy.
Integrated NTLM/Kerberos proxy authentication is not implemented; an authentication
challenge is reported rather than silently using the user's login.

The UI reports a short certificate/proxy/timeout/HTTP cause and preserves the draft.
The existing bounded diagnostic log records the full error chain. After a DNS/TCP/TLS/timeout failure, the explicitly approved alternate endpoint
`https://feedback.befeast.com/api/feedback/okilum` is attempted once. Any HTTP
rejection, malformed receipt, invalid issue link or local payload error stops
without fallback. Each attempt rebuilds multipart from the retained report;
streaming attachments are reopened rather than replaying a consumed body. Async
reqwest runs inside the existing background send operation so an early HTTP
rejection remains authoritative while a video is uploading; video bytes are
streamed from disk, and local file-read failures never trigger fallback. The
same 201/issue-link validation applies to both addresses. Only the successful
local attempt sets the UI delivery host and diagnostic delivery endpoint; these
cannot be supplied by the server. No infrastructure change is made by the app.

The tester subsequently updated successfully through About, so updater changes
are deferred. Velopack 1.2.161 has no agent injection point in HttpSource, but its
public UpdateSource extension permits a future transport change without forking
Velopack or replacing its checksum/install/restart logic.

Native acceptance uses an isolated Windows VM with a temporary test CA in the
current-user Root store, an inspecting proxy for the Feedback host, cleared proxy
environment variables and a controlled HTTP receipt. Negative controls remove
trust or use the old bundled-root/environment-only policy. Positive controls prove
static system proxy and PAC routing and capture the production client's User-Agent.
Mocks cover primary success, transport fallback, HTTP errors without fallback,
invalid receipts without fallback and the two-attempt maximum. No valid test
payload is sent to either public endpoint. Application before/after evidence must distinguish fixture delivery from the real
Feedback service and retain the source/artifact identity. Rollback removes the
fixture, trust, proxy configuration and test application state. This document is
a test plan and design record, not a claim of native acceptance.
