# Shared native Manager on Windows (#1788)

Windows mounts the retained native ProjectView used by Linux and Mac. The
shipping build enables `native_manager`, including the feed's external links,
the Settings pairing controls and native credential restoration. No WebView2
runtime, embedded browser or installer change is involved.

Pair through Settings or the Manager panel. The origin-bound `/ws` grant uses
GPUI's Windows credential store, separate from the optional `/mcp` grant. The
shared shell/thread subscriptions, message composer and inline selectable
questions send the same commands, including `runtime-request.respond`, on all
three platforms. Reopening the app restores that native grant for project and
Board gateways too. Open in T3 uses the actual server environment and selected
thread; a missing per-user `t3code` handler uses the configured server URL.

The existing product scope (#2203) displays the configured Manager conversation;
other conversations are reached through Open in T3. This platform change keeps
that shared scope. Rotation policy files retain immutable create-if-absent
publication and file flushes; Unix-only permissions/directory fsync follow the
same platform convention as the underlying portable PolicyStore. Windows has
no system TZif database at `/usr/share/zoneinfo`, so local scheduled rotation
configuration continues to report an unavailable timezone rather than silently
using another zone. It is independent of native chat and question responses.

Run the exact committed source through `H=github scripts/agent/gh-check.sh`
and the issue-scoped `windows-manager-native/**` hosted Windows workflow.
The Windows lane builds the production Reader without default features and
requires nonempty native transport, feed, questions, pairing, credentials,
configured-thread and desktop handoff tests. Its negative control removes only
Windows from the build cfg: the old fallback must compile and fail the shipping
constructor assertion. Exact build.rs bytes are restored before a passing rerun.
Hosted checks establish compilation and synthetic behavior. Delivered beta
acceptance requires the real Windows app, connection, conversation/reply,
selectable question/answer and Open in T3 evidence in #1788, plus a short
walkthrough; neither a green check nor a merge establishes that acceptance.
