# Embedded Manager load failures

The macOS Manager still mounts WKWebView, independently of the native
ProjectView. A created native view is not evidence that T3 loaded. Until
the native Manager replaces this path, each browser generation has two deadlines:

- The local preflight must report capabilities within 5 seconds.
- After requesting the T3 URL, a web document must finish within 20 seconds.
  The preflight's late `about:blank` completion does not count; redirects do
  not extend the deadline. Later committed navigations also get a deadline.

Timeouts and native navigation errors release the child view before showing
the existing fallback, retaining Open in T3, Open in browser and Retry.
Transient failures use the existing 2/5/10/30-second connection backoff; only
a completed web load resets the failure count. A definitive unsupported-engine
report waits for a manual retry or address change. Old-generation callbacks
and timers cannot affect a replacement or a closed panel.

`reader-diagnostic.log` records `manager_webkit_capability`,
`manager_page_load` (phase and URL), `manager_navigation_failure`
(native domain/code, blocked navigation or immediate load rejection),
`manager_webview_failure` (including deadline expiry), and
`manager_webview_occlusion` (hide/restore transitions, bounds and overlays).
URLs omit credentials, queries and fragments. Native failures are forwarded
through a per-view delegate proxy that preserves Wry's navigation policies,
downloads and page-load callbacks; cancellation (-999) is logged without
tearing down the replacement navigation. Process termination also falls back.

A zero-width or zero-height layout frame skips occlusion work. It does not
hide the native view. A real full overlay hides it until the first unobscured
paint; a failed bounds update releases it and enters fallback.

Pure watchdog tests cover deadlines, successful loads, late preflight events,
redirects, stale generations, close/reset, and diagnostic URL redaction.
The panel regression also checks fallback actions, quiet status, generation
invalidation and increasing retry counts. Hosted macOS checks compile the
production WebKit path and run the watchdog and native routing regressions.
These checks do not establish the cause of the reported white pane or verify
interactive T3 rendering on the affected Mac.
