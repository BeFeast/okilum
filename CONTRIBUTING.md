# Contributing to Okilum

## Reporting a bug

Open a [bug report](https://github.com/BeFeast/okilum/issues/new?template=bug_report.yml).
The form asks for what makes a bug reproducible:

- **Version info.** In Okilum open Settings → About and click **Copy** next to the
  version line, then paste it into the form. On Windows and Linux the ⋯ menu has
  **Copy version info**; on macOS it is Help → Copy Version Info. The line names the
  version, build, update channel, OS, architecture and how Okilum was installed.
- **Steps, expected and actual.** Start from opening Okilum. Say whether the vault is
  large or synced (Syncthing, iCloud, a network share): many problems depend on it.
- **A screenshot of the Okilum window.** For anything about timing (slowness,
  flicker, a click that seems to do nothing), a 10–20 second screen recording shows
  more than a description.
- **A log excerpt** from around the time it happened (see below).

Ideas and questions are welcome as ordinary [issues](https://github.com/BeFeast/okilum/issues) too.

## Which channel you are on

Settings → About shows the channel after the version, for example
«Stable channel». Stable is the default on every OS. Beta gets every change first
and is chosen explicitly. On Linux, Settings → About says which pacman repository
(`okilum-stable` or `okilum-beta`) updates come from.

The current release is at <https://okilum.app/download>.

## In-app feedback

Use the feedback icon next to ⋯ or Help → Send feedback. Write one message and,
if useful, attach a PNG/JPEG with the file picker, paste an image, or drop a file.
You can preview and remove the image before sending. Build, OS and window details
are filled automatically. This creates an issue in Okilum's public tracker; log
attachment is optional and off by default. A failed send keeps the draft while
its window stays open, with Retry and Copy report.

## Logs

Okilum keeps a diagnostic log of each launch: startup timings, warnings, failures
and crashes. It holds no note content, but it does name your vault folder and the
files you opened, so look it over before posting. The log rotates at 4 MiB and keeps
one older file next to it, `reader-diagnostic.log.previous`.

| OS | Diagnostic log |
|---|---|
| macOS | `~/Library/Application Support/com.befeast.okilum/reader-diagnostic.log` |
| Windows | `%LOCALAPPDATA%\okilum\reader-diagnostic.log` |
| Linux | `$XDG_STATE_HOME/okilum/reader-diagnostic.log`, or `~/.local/state/okilum/reader-diagnostic.log` when `XDG_STATE_HOME` is unset |

The last 250 lines are usually enough. On macOS or Linux:

```sh
tail -n 250 "$HOME/Library/Application Support/com.befeast.okilum/reader-diagnostic.log"  # macOS
tail -n 250 "${XDG_STATE_HOME:-$HOME/.local/state}/okilum/reader-diagnostic.log"           # Linux
```

On Windows, in PowerShell:

```powershell
Get-Content "$env:LOCALAPPDATA\okilum\reader-diagnostic.log" -Tail 250
```

The same folder holds Okilum's other state (window layout, recent vaults, unsaved
drafts). Please do not attach the whole folder.

For problems with updates on Windows, the updater writes
`%LOCALAPPDATA%\velopack\velopack_BeFeast.Okilum.log`. If Okilum crashes on macOS,
the system keeps a report named `okilum-….ips` in `~/Library/Logs/DiagnosticReports`.
On Linux, start `okilum` from a terminal and include what it prints.

## Pull requests

Pull requests are welcome. Development happens on a private Forgejo instance and
this repository mirrors `main` and releases, so accepted changes are landed there
and appear here with your authorship preserved. The [product contract](docs/PRD.md)
and [Reader design](docs/design/reader.md) describe how Okilum is meant to behave;
the [README](README.md#build-from-source) explains how to build it.
