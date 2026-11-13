# System terminal for a project (#2129)

Cmd+J (macOS), Ctrl+J (Linux/Windows) and Ctrl+backtick open the system
terminal for the active project. Manager options → Open terminal and the
palette invoke the same launcher. Settings → Keyboard and palette hints use
the live keymap. Input, Textarea and editor contexts retain their own keys:
Ctrl+backtick already toggles inline code in Source/Live Preview (#1636).
The Mac Manager browser routes native key events outside editable controls;
its composer retains T3's shortcuts. Page IPC cannot request a terminal.

A remote T3 origin opens `ssh -- <host>`. Loopback origins (including IPv6
and the full IPv4 loopback range) open the source checkout, or project folder.
A project without a T3 connection is local; a named but missing connection is
an explicit error. The origin uses the same project/connection/app fallback
as the Manager. An explicit `execution.terminal_host = "maestro"` overrides
the address-derived hostname, useful behind a reverse proxy. Hosts must be
valid IP addresses or DNS/SSH alias labels; options, userinfo, ports, whitespace
and shell syntax are rejected.

`execution.terminal_command` overrides the default. The palette's **Terminal
settings** saves a per-device override under application state
`project-terminal/`. Empty restores the manifest/default command. These files
survive index deletion and are removed with application state on uninstall.
For example, enter `ssh maestro` or `ssh -p 2222 maestro`. Quotes group argv
words; shell operators, substitutions and redirects are not evaluated.
Windows paths in an override should use single quotes (for example
`'C:\Program Files\MyTool\tool.exe' 'argument with spaces'`).

Linux selects executable xdg-terminal-exec, then the executable named by
TERMINAL with -e, then x-terminal-emulator -e. An unset, empty or unavailable
TERMINAL falls through; non-executable files do not count. TERMINAL names a
single executable, including paths with spaces. macOS uses osascript and
Terminal.app. Windows selects wt.exe then cmd.exe, with a PowerShell session.
Missing launchers and failed processes show a toast with the cause.

Commands remain argv data. Linux uses a fixed POSIX wrapper with positional
arguments; local launch changes directory before starting the interactive
shell. Terminal.app only accepts shell text, so each wrapper/argv word is
POSIX-quoted and passed as AppleScript argv, never embedded in AppleScript.
Windows passes only fixed switches and an encoded PowerShell program across
wt/cmd parsing: JSON carries argv/directory as base64 data, then the program
uses Set-Location -LiteralPath or ProcessStartInfo with explicit Windows argv
quoting, never eval. This also avoids PowerShell 5 stripping embedded quotes.
Process launch/wait runs outside the UI and shared worker pool, with bounded
stderr retention and child reaping. No discovery connects SSH or starts a
terminal, and no terminal is embedded.

Unit controls fake terminal availability on all platforms, check host and
command derivation, round-trip hostile argv through each platform encoding,
and execute only the fixed POSIX wrapper with harmless fake programs. GPUI
controls exercise actual shortcut dispatch, input conflicts, the shortcut
sheet and palette routing through an application-scoped fake launcher.
Native Mac key predicates and document editability have separate controls.
A visible terminal and successful remote login on idunn/macOS remain beta
acceptance checks; automated tests never open a real terminal.
