# Uninstall: what Okilum leaves on a machine (#974)

After uninstall the machine should look as if Okilum was never installed. The
only exception is the user's vaults: Okilum never deletes, moves or edits
anything inside a vault, and an app directory that contains a recorded vault is
kept rather than removed.

This file is the single inventory of every location Okilum writes. The test
`app_footprint::tests::every_base_directory_in_production_code_is_in_the_uninstall_inventory`
fails when production code computes a new base directory; add the location here
and to `app_footprint::roots()` (or the OS-specific removal) before updating it.

## Shared rules

- **Roots** (`app_footprint::roots()`, computed by the same functions the app
  writes through):
  - the state directory (`reader_history::state_directory`, honours `OKILUM_STATE_DIR`);
  - `<config>/okilum` (`reader_layout::config_base`);
  - `<cache>/okilum`, the default search index (`reader_open::cache_base`);
  - on Unix, `${XDG_CONFIG_HOME:-~/.config}/okilum` (Brain profile and outboxes).
- **Temp:** `<tmp>/okilum-search-session-*`, one per run. A crash leaves one behind;
  startup sweeps those older than a day, uninstall removes all.
- **Vault guard:** vault roots recorded in `update-session.json` (`recent_roots`,
  `quick_roots`, `last_documents`) and `reader-ui.json` (`vaults`,
  `vault_colors`) are read first, with Windows `\\?\` / `\\?\UNC\` prefixes
  stripped. A root that contains one of them, or any Markdown file or `.obsidian`
  folder even without a record, is reported and kept.
- **Unsaved drafts:** `<state>/editor-drafts/*.json` whose `text` differs from the
  file on disk, or whose file is gone, are exported to
  `Documents/Okilum unsaved drafts/` before the state is removed. Drafts equal to
  the saved file are dropped.
- **Inside vaults:** `.okilum-save-*` / `.okilum-create-*` transient files may
  remain after a crash. They are part of the vault and are never deleted
  automatically.

## Windows (Velopack, per user)

Settings → Apps → Okilum → Uninstall runs Velopack's uninstaller. Its
before-uninstall hook (`app_footprint::uninstall`) removes everything below that
Velopack does not.

| Item | Path / key | Removed by |
|---|---|---|
| Install root | `%LOCALAPPDATA%\BeFeast.Okilum\` (`Update.exe`, `Okilum.exe`, `current\`, `packages\` update cache) | Velopack |
| Uninstall entry | `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\BeFeast.Okilum` | Velopack |
| Shortcuts | Start menu and desktop `Okilum.lnk` | Velopack |
| Markdown Open With | `HKCU\Software\Classes\BeFeast.Okilum.Markdown`, value `BeFeast.Okilum.Markdown` in `HKCU\Software\Classes\.md\OpenWithProgids` | hook (`markdown_handler::uninstall`) |
| Notification identity | `HKCU\Software\Classes\AppUserModelId\com.befeast.okilum` | hook |
| State and search index | `%LOCALAPPDATA%\okilum\` (state files, `editor-drafts\`, `link-moves\`, `sidebar\`, `reminders\`, `reader\<sha>\` index, `reader-diagnostic.log`, `windows-update-channel`) | hook |
| Config | `%APPDATA%\okilum\` (`reader-layout.json`, `appearance.json`, `reader-runs\`) | hook |
| Search sessions | `%TEMP%\okilum-search-session-*` | hook; startup sweeps stale ones |
| Crash dumps | `%LOCALAPPDATA%\CrashDumps\okilum.exe.*.dmp` | hook |
| Velopack's per-app log | `%LOCALAPPDATA%\velopack\velopack_BeFeast.Okilum.log` (Velopack writes it after the hook); the `velopack` folder only if then empty | hook, by a detached cleanup that waits for `Update.exe` to exit |

There is no Run key, URL protocol, scheduled task, named pipe, service or
Credential Manager entry. The sync sidecar's task and pipe exist in code but are
not shipped on Windows.

Acceptance on a clean profile: `scripts/uninstall/windows-snapshot.ps1 -Out before.txt`
before install, use the app, uninstall, snapshot again, then
`scripts/uninstall/diff.py before.txt after.txt --vault <vault>`. It lists every
added entry and fails on anything named after Okilum outside the vault.

Known residue owned by Windows or Velopack, not by Okilum: Velopack's shared log
`%LOCALAPPDATA%\velopack\velopack.log` (only written by `Setup.exe`), Explorer's `MuiCache` and `Recent`
entries. The acceptance snapshot lists them separately.

## Linux (Arch package `okilum`)

Remove in this order, as yourself:

```sh
okilum --uninstall-data     # per-user data; asks once, keeps your notes
sudo pacman -Rns okilum     # the package
```

- **Package files**, removed by pacman: `/usr/bin/okilum`,
  `/usr/share/applications/okilum.desktop`,
  `/usr/share/icons/hicolor/*/apps/okilum.*`, `/usr/share/licenses/okilum/`.
  The package's `okilum.install` lists the remaining per-user folders on
  `post_remove` (the binary is already gone by then). pacman runs as root and cannot reach each
  user's home, so it does not delete per-user data itself.
- **Per user**, removed by `okilum --uninstall-data` (`--yes` skips the one
  confirmation):
  - `${XDG_STATE_HOME:-~/.local/state}/okilum/`, `${XDG_CONFIG_HOME:-~/.config}/okilum/`
    (Brain profile and outboxes too), `${XDG_CACHE_HOME:-~/.cache}/okilum/`;
  - `/tmp/okilum-search-session-*` and `/tmp/okilum-sync-enrollment-<uid>`.
- **Refuses, removing nothing,** while Okilum runs (instance lock held) or while
  Sync is set up: an active `sync/setup.json` or an `okilum-syncthing-*.service`
  user unit. Sync must be removed in Okilum → Settings → Sync, so the hub forgets
  this computer and the service is stopped by the controller, never by deleting
  files.
- `okilum.desktop` in `~/.config/mimeapps.list`, if the user chose Okilum as the
  default for Markdown, is the user's own choice and stays. It is harmless once
  the package is gone.

## macOS (`com.befeast.okilum`)

Okilum → Settings → About → **Uninstall Okilum…** shows one confirmation with
what goes and which vaults stay, then:

1. exports unsaved drafts to `~/Documents/Okilum unsaved drafts/` and removes the
   roots: `~/Library/Application Support/com.befeast.okilum/` (state),
   `~/Library/Application Support/okilum/` (config), `~/Library/Caches/okilum/`
   (search index), `~/.config/okilum/` (Brain), `$TMPDIR/okilum-search-session-*`;
2. quits, and a detached cleanup waits for the process to exit, then removes:
   - preferences with `defaults delete <bundle id>` (cfprefsd caches them, so the
     plist alone is not enough) and `~/Library/Preferences/<id>.plist`;
   - `~/Library/Saved Application State/<id>.savedState` (written by AppKit on quit);
   - `~/Library/HTTPStorages/<id>[.binarycookies]`, `~/Library/WebKit/<id>`;
   - `~/Library/Caches/<id>/` (Sparkle's update cache);
   - `~/Library/Logs/DiagnosticReports/okilum*`;
3. moves `Okilum.app` to the Trash. If the user cannot write to `/Applications`,
   the app stays there; dragging it to the Trash finishes the job.

The bundle id is read from the app's `Info.plist` (a build that is not inside an
`.app` bundle cleans nothing after quitting), so the QA build
(`com.befeast.okilum.intel-qa`) cleans its own domain; anything that is not
`com.befeast.okilum[.*]` is left alone. Moving the app to the Trash alone
cannot clean these up. There are no LaunchAgents, keychain items or URL
schemes; LaunchServices forgets the document types with the app.
