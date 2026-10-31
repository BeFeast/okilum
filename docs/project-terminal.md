# System terminal for a project (#2129)

Open terminal uses the active project's T3 connection: a remote origin opens
`ssh <host>`, while loopback/local projects open the configured source checkout
(or project directory). A missing remote connection is an explicit error, never
an assumed local checkout. A reverse-proxy hostname is not silently translated
into an SSH alias: projects can set `execution.terminal_host = "maestro"` in their
existing TOML/YAML settings. `execution.terminal_command` supplies an explicit
initial shell command. Only a user invocation runs it.

The palette also exposes **Terminal settings** for a per-device project override.
An empty override restores the manifest/default command. Overrides live under
application state `project-terminal/`, so uninstall removes them with that state;
they do not rewrite a shared project manifest or contain T3 credentials.

Cmd+J (macOS), Ctrl+J (Linux/Windows) and Ctrl+backtick invoke the same action as
Manager options → Open terminal and the palette. Bindings exclude Input,
Textarea and editor contexts to preserve their own shortcuts. Settings → Keyboard
and palette hints use the live keymap. A native text field that consumes a key
retains it; no OS-wide keyboard hook is installed.

Linux uses xdg-terminal-exec, then the executable named by TERMINAL with -e,
then x-terminal-emulator -e. macOS passes the command as an osascript argument
to Terminal.app, never interpolated AppleScript. Windows uses wt.exe with a
PowerShell tab, falling back to cmd/start. Defaults quote the SSH host or local
path as data; a custom command intentionally uses the platform shell syntax.
No terminal is embedded, no SSH session is opened by discovery, and spawn errors
appear as a toast.

Acceptance on idunn and macOS belongs to the normal beta UI check. Unit controls
exercise platform argv, metacharacter quoting and malformed SSH destinations;
they do not claim a human terminal window or successful remote login.
