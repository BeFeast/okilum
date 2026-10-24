# Windows Feedback transport (#1667)

Feedback uses `okilum-http` for client policy while retaining its multipart
payload, timeouts, no-redirect policy and public endpoint. Windows explicitly
selects reqwest's SChannel/native-TLS backend, which validates against the OS
certificate store. Bundled rustls roots remain the policy on other platforms.
No certificate or hostname verification is disabled.

Windows proxy discovery uses the current user's Internet settings and resolves
PAC/WPAD through WinHTTP for the request URL. Explicit proxy environment variables
retain their previous precedence; system bypass rules and NO_PROXY are honored.
PAC fetching does not automatically authenticate with Windows credentials.
Integrated NTLM/Kerberos proxy authentication is not implemented; an authentication
challenge is reported rather than silently using the user's login.

The UI reports a short certificate/proxy/timeout/HTTP cause and preserves the draft.
The existing bounded diagnostic log records the full error chain. No fallback
Feedback domain or infrastructure change is introduced.

The tester subsequently updated successfully through About, so updater changes
are deferred. Velopack 1.2.161 has no agent injection point in HttpSource, but its
public UpdateSource extension permits a future transport change without forking
Velopack or replacing its checksum/install/restart logic.

Native acceptance uses an isolated Windows VM with a temporary test CA in the
current-user Root store, an inspecting proxy for the Feedback host, cleared proxy
environment variables and a controlled HTTP receipt. Negative controls remove
trust or use the old bundled-root/environment-only policy. Positive controls prove
static system proxy and PAC routing and capture the production client's User-Agent.
Application before/after evidence must distinguish fixture delivery from the real
Feedback service and retain the source/artifact identity. Rollback removes the
fixture, trust, proxy configuration and test application state. This document is
a test plan and design record, not a claim of native acceptance.
