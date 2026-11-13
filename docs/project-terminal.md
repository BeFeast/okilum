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
single executable, including paths with spaces. macOS uses
`/usr/bin/osascript` and Terminal.app. Windows selects Windows Terminal, else
the System32 Windows PowerShell in a new console; cmd.exe is never used.
That console is created with CreateProcessW without standard handles
(no STARTF_USESTDHANDLES) and without handle inheritance, so PowerShell's
stdin/stdout are the new console and `-NoExit` stays interactive; std's
`Command` always passes handles, which a new console does not replace.
PowerShell profiles stay enabled: it is the user's interactive shell.
Missing launchers and failed processes show a toast with the cause.

Every launcher is an absolute program from a trusted location (#2837 review):
an absolute TERMINAL value must exist; `powershell.exe` comes only from the
system directory (GetSystemDirectoryW) and is passed to wt as an absolute
path; other names (wt.exe, including its App Execution Alias, and the Linux
terminals) come only from absolute PATH entries outside the vault, the
selected folder and the project folder, including entries that link there.
Relative, empty and `.` entries and names with a directory part are ignored,
so a synced `powershell.exe`, `wt.exe` or `xdg-terminal-exec` next to a note
never runs. The launcher keeps Okilum's own working directory; folders travel
only as arguments.

Commands remain argv data. Linux uses a fixed POSIX wrapper with positional
arguments; local launch changes directory before starting the interactive
shell. The interactive shell is SHELL, or the account's login shell from
`getent passwd` when the terminal starts without SHELL (xdg-terminal-exec
through uwsm/systemd-run on Omarchy, #2837), looked up before changing
directory; an empty, `nologin`, `false` or `true` shell becomes /bin/sh. Terminal.app only accepts shell text, so each wrapper/argv word is
POSIX-quoted and passed as AppleScript argv, never embedded in AppleScript.
Windows passes only fixed switches and an encoded PowerShell program across
wt parsing: JSON carries argv/directory as base64 data, then the program
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

## Open in terminal for a file or folder (#2837)

The file action **Open in terminal** opens an interactive terminal in a vault
folder, or in a file's containing folder; no command runs in it and nothing
connects to a remote host. It reuses this launcher's process handling and
error toast, not the project command or T3 host. Surfaces and the shortcut are
listed in docs/keyboard-shortcuts.md and docs/design/reader.md.

Linux tries xdg-terminal-exec, TERMINAL and x-terminal-emulator as above (the
fixed POSIX wrapper changes into the folder), then known emulators with the
desktop's own first (GNOME: ptyxis, kgx, gnome-terminal; KDE: konsole; Xfce:
xfce4-terminal), each given one `--working-directory=<folder>`-style word.
macOS runs
`/usr/bin/open -a Terminal <folder>`; Okilum has no terminal-app preference
yet, so Terminal.app is used. Windows runs `wt.exe -d <folder>`; a folder
containing `;` (which wt treats as a command separator) and the PowerShell
fallback use the encoded PowerShell route above, where the folder is base64
data for `Set-Location -LiteralPath`. Folders are absolute, so a name starting
with `-` or `@` is never an option or a splat, and each folder is one argv
element without shell interpolation. On Linux and macOS the folder stays raw
OS bytes, so a path that is not UTF-8 opens too; only the PowerShell route
needs Unicode text and reports such a folder. Windows paths drop the `\\?\`
prefix.

Launched from an AppImage, the terminal does not inherit the bundle's
environment: APPIMAGE, APPDIR, ARGV0, OWD, APPIMAGE_EXTRACT_AND_RUN and
OKILUM_APPRUN_PRELOAD are removed. AppRun prepends the host ALSA loader to
LD_PRELOAD and names it in OKILUM_APPRUN_PRELOAD; exactly that entry is
removed. Entries inside APPDIR are dropped from LD_LIBRARY_PATH, LD_PRELOAD,
PATH, XDG_DATA/CONFIG_DIRS and the GTK, GIO (including GIO_EXTRA_MODULES),
GDK-Pixbuf, GStreamer, Qt (including QT_QPA_PLATFORM_PLUGIN_PATH), Python
(PYTHONHOME, PYTHONPATH), Perl, fontconfig, GL/VA/EGL/Vulkan driver and TLS
certificate variables. A value the user set outside the bundle is kept, in
order; Okilum's AppRun sets nothing else. This applies to Open terminal too.

Unit controls build every platform's command with hostile folder names
(spaces, quotes, `$()`, `&`, `;`, non-ASCII, non-UTF-8, leading `-`/`@`),
execute the POSIX wrapper with a fake shell and a fake passwd lookup, place
decoy launchers in the vault, the folder and relative PATH entries, assert
that no Windows plan contains cmd.exe, `start` or a bare PowerShell name and
that the launcher process has no working directory of its own, and check the
AppImage environment edits, including a mixed colon/space LD_PRELOAD. The
native Windows job (`scripts/ci/windows-terminal-console.py`) compiles the
production console spawner, starts the System32 PowerShell from a working
directory holding a decoy `powershell.exe`, and requires console stdin/stdout,
the System32 image and the folder at the first interactive prompt; the #2858
spawn (std handles plus a new console) and `cmd /c start powershell.exe` from
the folder must fail those checks.
A GPUI control reaches an application-scoped fake from the sidebar menu,
shortcut and palette. A visible terminal on each OS remains beta acceptance.
