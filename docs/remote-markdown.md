# Read-only remote Markdown (#1610)

An explicit `ssh://host-alias/path/file.md` link opens a separate, window-owned
read-only Markdown view. The controller reuses the existing bounded SFTP
transport and system OpenSSH, including the configured host alias, ssh-agent
and known host key. This slice requires an already usable noninteractive alias:
URL userinfo/port options and authentication dialogs are unsupported.

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

This delivers the read-only quick-view portion of #1610. Remote editing, atomic
write-back/conflict recovery, authentication UI, external protocol registration
and remote folders/vaults remain separate outcomes. Headless fixtures use only
in-memory transports; live SSH, native window appearance and network acceptance
are not established by the source checks.
