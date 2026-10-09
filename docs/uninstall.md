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
  stripped. A root that contains one of them is reported and kept.
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

There is no Run key, URL protocol, scheduled task, named pipe, service or
Credential Manager entry. The sync sidecar's task and pipe exist in code but are
not shipped on Windows.

Acceptance on a clean profile: `scripts/uninstall/windows-snapshot.ps1 -Out before.txt`
before install, use the app, uninstall, snapshot again, then
`scripts/uninstall/diff.py before.txt after.txt --vault <vault>`. It lists every
added entry and fails on anything named after Okilum outside the vault.

Known residue owned by Windows or Velopack, not by Okilum: Velopack's shared log
`%LOCALAPPDATA%\velopack\velopack.log`, Explorer's `MuiCache` and `Recent`
entries. The acceptance snapshot lists them separately.

## Linux (Arch package `okilum`) — next PR

- Package files, removed by `pacman -Rns okilum`: `/usr/bin/okilum`,
  `/usr/share/applications/okilum.desktop`,
  `/usr/share/icons/hicolor/*/apps/okilum.*`, `/usr/share/licenses/okilum/`.
- Per user: `${XDG_STATE_HOME:-~/.local/state}/okilum/` (including `sync/`),
  `${XDG_CONFIG_HOME:-~/.config}/okilum/`, `${XDG_CACHE_HOME:-~/.cache}/okilum/`,
  `/tmp/okilum-search-session-*`, `/tmp/okilum-sync-enrollment-<uid>`, the
  `okilum-syncthing-<id>.service` user unit (removed through the sync
  controller, never with `rm`), and `okilum.desktop` in `mimeapps.list` if the
  user chose it.
- Planned: `okilum --uninstall-data` removes the per-user part with the same
  rules, and the package's post-remove message points to it. pacman cannot reach
  each user's home.

## macOS (`com.befeast.okilum`) — last PR

- `/Applications/Okilum.app`.
- `~/Library/Application Support/com.befeast.okilum/` (state) and
  `~/Library/Application Support/okilum/` (config), `~/Library/Caches/okilum/`.
- Sparkle's cache under `~/Library/Caches/com.befeast.okilum/`.
- `~/Library/Preferences/com.befeast.okilum.plist` (`OkilumReceiveBetaBuilds`,
  Sparkle `SU*`).
- `~/.config/okilum/` (Brain).
- Saved Application State, HTTPStorages and DiagnosticReports for the bundle id.
- Planned: Settings → "Uninstall Okilum…" with one confirmation, plus a documented
  command. Moving the app to the Trash alone cannot clean these up.

## Legacy Tessera leftovers

Okilum starts from clean folders and does not read Tessera's. Removing old
Tessera data (`tessera` dirs, `BeFeast.Tessera.*` keys, `uk.oklabs.tessera`) is a
separate, explicit offer and not part of Okilum's uninstall.
