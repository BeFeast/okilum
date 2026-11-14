# Remote Markdown quick view (#1610)

An explicit `ssh://[user@]host[:port]/path/file.md` link opens a separate,
window-owned Markdown view. The host may be an `~/.ssh/config` alias. The
controller reuses the existing bounded SFTP transport and system OpenSSH, so
aliases, `IdentityFile`, `ProxyJump`, ssh-agent, `known_hosts` and ControlMaster
behave as in a terminal. A user and port from the link become separate `-l` and
`-p` arguments; a password in the link is refused.

## Authentication

ssh-agent and keys go first; OpenSSH keeps every decision. When OpenSSH itself
needs an answer (key passphrase, password, keyboard-interactive/2FA code, or
trust for an unknown host) it runs `SSH_ASKPASS` with
`SSH_ASKPASS_REQUIRE=force`. That program is the Okilum binary: it detects
`OKILUM_SSH_ASKPASS_SOCKET` at the very start of `main`, sends the prompt over a
private Unix socket (in a `0700` directory) to the running app and prints the
answer. The app shows an "SSH authentication" or "SSH host verification"
window; Cancel, Escape or closing it answers nothing, and further prompts from
that same connection are cancelled without another window. Trusting a host
answers `yes`, so OpenSSH records the key in `known_hosts` and does not ask
again; a changed host key is still refused by OpenSSH. Answers are never
stored, logged or reused. Time spent in the window does not count against the
read, listing or save deadlines.

Only remote Markdown targets are interactive. SFTP log following reconnects on
every poll and stays `BatchMode=yes`, alias-only. On Windows the bridge is not
installed yet, so connections stay non-interactive (agent/keys only). Each new
connection (open, folder listing, save) may ask again unless OpenSSH reuses a
ControlMaster connection from the user's own configuration.

The view accepts at most 2 MiB of strict UTF-8 source. Reads and preparation run
on background workers, with cancellation and the total deadline checked around
every bounded transport operation. A stopped, replaced or closed view cannot
publish an older generation's reply. Remote source stays session-only and never
enters a local vault, FileEditor, source editor or saved Reader history. The
view exposes loading, read-only, cancelled and disconnected states.

Relative Markdown links resolve from the captured remote document directory;
absolute paths remain remote paths. Heading intent uses the same rendered
Markdown's HeadingInventory, with explicit missing/ambiguous diagnostics. A link
from an inactive document cannot navigate the current one. Remote HTML is shown
literally and media is unavailable; rendering receives no authority to read a
local file or fetch remote assets. It does not resolve remote relative paths
against the open local vault.

Guarded save and bounded folder browsing are described in their modules
(`remote_markdown::save`, `remote_markdown::folder`). Registering Okilum as the
system handler for `ssh://` links and remote vaults are out of scope (Oleg,
23.10): links clicked inside Okilum open the view. Headless fixtures use only
in-memory transports; live SSH, native window appearance and network acceptance
are not established by the source checks.
