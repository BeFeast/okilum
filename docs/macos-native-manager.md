# Native Manager on Mac (#2516)

macOS mounts the same retained ProjectView as Linux. Its native textarea,
feed, WebSocket v2 subscriptions, scoped thread selection, drafts, model
catalog and connection state no longer depend on T3's browser application.
The Manager webview, cookie export, focus bridge, retry/preflight code and
embedded Settings sign-in are removed. Existing browser storage is untouched.

## Connection and parity

An existing webview session is not a native enrollment. Connect explicitly
using the native pairing controls. The origin-bound native bearer is stored
under `/ws` in OS credentials (Keychain on Mac), independently of the optional
MCP credential under `/mcp`. Restoring a project reads both audiences through
the same gateway used by Board, Projects, icon read/save/reset and summaries.
A refused native grant stays visible and cannot fall back to a broader grant.
Installing a paired client immediately refreshes the Board/project gateways.

Enter sends; Shift+Enter inserts a newline; marked text and its commit settle
interval suppress send. Cmd shortcuts and Edit actions reach the focused
composer or feed selection. PNG/TIFF paste, Finder file URLs, file drop and
multi-select attachments use the shared bounded preparation pipeline. See
[the input contract](macos-manager-input.md) for hosted versus live coverage.

Board → task → Discuss with Manager appends context to the visible native
composer, preserves its existing draft and never sends automatically. Task
and Chat tabs keep the same Manager entity. Closing the panel revokes the
handoff target. The retained chrome and Board Manager display cache work on
both native platforms; redraws do not rebuild the Board source.

Open in T3 targets `t3code://threads/<environmentId>/<threadId>` for the current
thread, using NSWorkspace to check the registered handler on Mac. If no handler
exists, it opens the configured T3 server's thread URL in the default browser.
No protocol registration is changed. Full T3 diff, terminal, checkpoint,
worktree, account and administration surfaces remain in T3.

The model picker uses the existing connection-warmed scoped catalog: opening
or closing it does not request a catalog. Its shared regressions remain in the
hosted native gate. No new server API or production session is required.

## Timing and verification

`manager_ttfm` in `reader-diagnostic.log` records one event per native panel
open after a message row reaches visible paint. Cold success requires both
shell and thread snapshots. A missing snapshot cannot log successful TTFM.
Warm reopening records retained-client reuse and explicitly null unobserved
phases; it does not claim physical socket reuse. See
[stream timing](manager-stream-transport.md) for phase definitions.

Reader panel visibility owns this measurement separately from project
selection activation (#2693). Hiding the retained Manager cancels its pending
paint probe immediately; reopening invalidates the cached feed and starts a
fresh measurement. Empty feeds, stale callbacks and repeated redraws cannot
produce a successful new event.

Expanded connection Details shares a bounded scroll region with connection
and pairing controls (#2694). It reserves at most 40% of the Manager body's
height, clips wrapped diagnostic content, and leaves the transcript and
composer in separate layout regions. Expanding or collapsing Details does
not replace the retained feed, draft, native client or binding.

Compare Mac/Linux on the same committed source, and before/after on the same
machine in the same session. Hosted GPUI timings are regression evidence,
not the installed Mac's time-to-first-message. A build or green CI alone is
not live acceptance.

Before merge, run the exact committed HEAD through `gh-check` and the hosted
macOS lane. The latter compiles the shipping Reader and every workspace test
target and executes all native Manager modules, gateway/navigation and Board
Discuss controls. Tests use synthetic projects, fake credentials and isolated
pasteboards; the development project is never a product test goal.

Mac runtime acceptance follows project authority. Do not restart the stopped
Mac harness or automate Oleg's installed app without its applicable authority.
Keep the epic open until the beta is delivered, installed-surface timing and
Open in T3/parity are verified, and a 1–3 minute walkthrough and tester note
are available. Record build/source and unverified items explicitly.

## Short tester walkthrough

1. Open a project and pair the native Manager once if upgrading from the old
   browser session. Reopen it to check the remembered connection.
2. Type a draft, use Shift+Enter, paste an image and attach a file. Verify
   Cmd/Edit actions, IME composition and Finder drop on the active Mac.
3. Open a Board task and choose Discuss with Manager. The context joins the
   existing draft in Chat, without sending.
4. Open the model list and switch threads; confirm independent drafts.
5. Choose Open in T3 and confirm that the same thread opens in T3 desktop.
6. Record a cold and warm `manager_ttfm` event for this exact beta, plus its
   build/source. A screen recording must demonstrate the real app; hosted
   fixtures are not substituted for the walkthrough video.
