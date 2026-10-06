# Inbox browser QA

Run the real web modules against deterministic API fixtures in an isolated
Playwright context on the shared Forge CDP browser. No source actions, owner
cookies or existing tabs are used. Screenshots are design fixtures, not proof
of live passkey/backend acceptance. Tests cover 390/1280 light/dark, project
chips, inline status edits, filters/details, guarded reply, capture and offline
outbox synchronization. Detail checks cover tap choices/custom answers, automatic source
refresh without duplicate sends, chat message sending, and the publication sheet:
no initial errors, explicit validation on Publish, and a guarded publication. Native API/auth/store tests remain a separate gate.

Install with `npm ci` in `web/inbox`. Serve that directory from a temporary LAN
HTTPS server (a self-signed certificate is accepted by this isolated context).
Then run from the repository root:

```sh
QA_BASE_URL=https://10.10.0.23:18770/ \
QA_CDP_ENDPOINT=http://10.10.0.50:18811 \
QA_ARTIFACT_DIR=target/inbox-browser-qa \
node web/inbox/qa/browser.mjs
```

Use a provisioned CDP endpoint; do not restart the shared browser. The script
closes only its own contexts and disconnects on completion. Fixtures intercept
all `/api/v1/` traffic and fail on unhandled endpoints or JavaScript errors.
