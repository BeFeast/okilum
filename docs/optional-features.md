# Optional features (#2091, #2092)

Okilum starts as a Markdown editor/viewer. Settings → Features contains Agents
(Projects/Board/Manager/T3), Second brain and Sync. A new profile starts with all
three off, including profiles with old connection records but no feature choices.
Enabling a module reveals its existing entry points immediately; connection/setup
stays at first use. Disabling hides those entry points without deleting notes,
credentials, pairing records, project manifests, retained messages or drafts.

Second brain controls the existing managed Workspace on supported builds. The
future Now sidebar (#1927) must observe the same feature; it is not implemented or
replaced by a placeholder here. Modules not shipped on a platform have a disabled
switch and an availability explanation. Existing OS-managed Sync enrollment is
kept; this preference stops Okilum's controller/requests, not an independently
installed system service or another application's Syncthing process.

Choices live in `features.json` in application state, outside vault metadata.
Writes merge one flag under a file lock and replace the file atomically. An
unreadable profile fails closed and stays untouched. If saving an Off choice
fails, access is still revoked locally and the persistence error is shown.

## Lifecycle boundary

`reader_features::enabled`, `access` and the observable `Features` global are the
shared API. All production startup paths install off defaults before opening a
Reader or managed surface. Component fixtures explicitly enable the modules they exercise; profile tests
install the production store. Missing preferences always deny access.

Every enabled period owns a distinct revocable `NetworkAccess`. Clients retain
that permission, and executor futures carry it through thread-local scopes for
blocking descendants. Turning a module off revokes the old period; enabling it
again cannot revive stale work. HTTP/MCP, browser-session, native WS, OAuth,
managed-brain RPC and Sync requests check permission at the transport boundary.
Already issued IO may finish; subsequent requests/reconnects are refused. Reader
selection and editor/search work do not depend on module connectivity. Manager
sockets close, panel navigation is released, polling stops and late Sync results
are discarded. Ordinary update checks and explicit remote document/link actions
are independent core features and are unaffected.

Regressions cover default/persisted choices, malformed storage preservation,
failed-save local revocation, actual Settings clicks, lazy/disabled Projects with
an enabled positive control (#1521), retained HTTP/native/Sync clients, RPC
permission and scope restoration across executor threads.

## Contextual Agents offer (#2093)

With Agents off, an opened folder with a Git checkout (including worktree `.git`
files), a root project manifest, a code/build manifest or top-level source files
receives one inline “Enable Board and Manager?” offer. A running local T3 Code
executable is also a relevance signal. Detection reads local filesystem/process
metadata on a worker; it does not start processes, inspect command lines or
credentials, probe ports, resolve hosts or contact T3. A plain notes folder with
no running T3 Code has no offer. Single-file windows have no offer.

The offer occupies a fixed footer slot in the sidebar, so its arrival and dismissal
do not move the tree. “Not now” persists for the opened folder using its existing
sidebar state key; older snapshots cannot undo that answer. Settings → Features
remains available after declining.

“Enable” persists Agents on and opens a local empty Board without an onboarding
form. An existing project/Board is reused. Otherwise the folder gets the existing
local project manifest/index creation and a Board with its folder name as title,
no repository, stage note or cards. Existing notes, manifests and Boards are not
rewritten. No T3 project, thread or connection is created by this action. A failed
profile save leaves Agents off; a local setup error is shown without automatic
retry or claiming that Board setup succeeded.
