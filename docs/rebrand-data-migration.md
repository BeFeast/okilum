# Tessera → Okilum: app-state import (#967)

On its first launch an Okilum build imports the user's Tessera app state into
Okilum locations. The legacy directories are only read: they stay where they
are as a backup until a later, explicit cleanup step. Canonical Markdown, vault
paths and Syncthing folder/device IDs are never renamed for the brand.

Code: `tessera_core::app_migration` (`roots`, `import`, `first_launch`). This
change is additive: `APP_BRAND` stays `Tessera`, so nothing runs in today's
builds. The rename (#970) flips `APP_BRAND` and calls `first_launch()` before
the Reader takes its own instance lock.

## Locations

`S` = state, `C` = Reader config, `B` = Brain config. Environment overrides
(`XDG_*`) are honoured exactly as the legacy helpers resolve them.

| Root | OS | Tessera (old) | Okilum (new) |
|---|---|---|---|
| S | macOS | `~/Library/Application Support/uk.oklabs.tessera` | `~/Library/Application Support/com.befeast.okilum` |
| C | macOS | `$XDG_CONFIG_HOME/tessera`, else `~/Library/Application Support/tessera` | `…/okilum` |
| B | macOS | `$XDG_CONFIG_HOME/tessera`, else `~/.config/tessera` | `…/okilum` |
| S | Linux | `$XDG_STATE_HOME/tessera`, else `~/.local/state/tessera` | `…/okilum` |
| C, B | Linux | `$XDG_CONFIG_HOME/tessera`, else `~/.config/tessera` (one tree) | `…/okilum` |
| data | Linux | `$XDG_DATA_HOME/tessera`, else `~/.local/share/tessera` (older Brain deployments) | `…/okilum` |
| S | Windows | `%LOCALAPPDATA%\tessera` | `%LOCALAPPDATA%\okilum` |
| C | Windows | `%APPDATA%\tessera` | `%APPDATA%\okilum` |
| cache | all | `~/Library/Caches/tessera`, `$XDG_CACHE_HOME/tessera` or `~/.cache/tessera`, `%LOCALAPPDATA%\tessera` cache | not imported; the index is rebuilt |

When macOS has `XDG_CONFIG_HOME` set, C and B are one tree and are imported
once. Brain stores are Unix-only. An explicit `OKILUM_STATE_DIR` /
`TESSERA_STATE_DIR` is an isolated, caller-chosen location: it is used as is
and never migrated. The Windows install root (Velopack `BeFeast.Tessera` →
`BeFeast.Okilum`) is separate from these data roots.

What the state root holds and therefore moves: last-document history, UI
preferences, window frames, per-vault sidebar, templates and reminder settings,
reminder delivery ledgers (so no notification repeats), editor drafts, source
history and link-move journals, the macOS FSEvents replay cursor, the Windows
update channel, crash-run markers, the diagnostic log and, on Linux, the sync
pairing file. The config roots hold panel layout, appearance, the Brain
workspace, prepared operations, outboxes and Brain editor recovery.

## Rules

- **Copy-first, atomic per root.** A new root that does not exist yet is copied
  into a sibling `.okilum-import-<name>-<id>` staging directory; every file is
  copied through a temporary file, fsynced and compared by SHA-256; a manifest
  (`okilum-import.json`) is written; staging is renamed into place in one step.
  An interrupted import leaves only staging, which the next launch discards and
  redoes.
- **Populated new root.** Missing files are added. A legacy file that differs
  from the existing new file is kept under `okilum-import-conflicts/` and listed
  in the manifest; the new file wins. An interrupted merge resumes without
  duplicating anything.
- **Idempotent.** The manifest is written last; with it present a root is not
  touched again, so settings changed in Okilum are never rolled back.
- **Never imported:** lock files, sockets, symlinks, temporary and pending files,
  `reader-instance.json` (a live process endpoint). They are listed as skipped.
- **One writer.** The import refuses while a legacy Tessera holds
  `reader-instance.lock` or a live `reader-runs/*.active` marker in the legacy
  state root (an OS file lock, released when the process exits), and two
  imports cannot run at once (`okilum-import.lock` beside the new state root).
  The legacy lock file is never created by the probe.
- **Environment variables.** `env_var("OKILUM_…", "TESSERA_…")`: the new name
  wins; the legacy name is still honoured when the new one is unset.

## Not covered here

- In-vault markers (`.tessera-save-*`, `.tessera-create-*`, `.tessera-index`),
  internal `tessera://` / `tessera-asset://` links and persisted schema names:
  the rename (#970) must keep reading the legacy forms; this module does not
  touch vault contents.
- macOS Sparkle preferences (NSUserDefaults domain `uk.oklabs.tessera`), OS
  notification permissions and the Windows Markdown ProgID belong to the
  installer/bridge work (#969, #971).
- Sync supervisors (Linux units, macOS launchd, Windows tasks/pipes) must be
  re-registered under one owner by the rename; the pairing file itself moves
  with the state root.

## Native check

`cargo run -p tessera-core --example okilum_import -- plan | run [--fail-after N]`
uses the real environment of the machine it runs on. Run it on tessera-dev
(CT141) with a sandbox `HOME`, never on maestro.
