# Settings inventory

Source baseline: `ebc805082bfe52c91c6516cf36903d54cd5cf69a` (20 November 2026). Static source inspection; this is not a native UI acceptance report.

All source paths below are relative to `crates/okilum-shell/src/`. Unless noted, settings are saved app-wide in `<state>/reader-ui.json`, in the `Saved` struct at `reader_ui_state.rs:132`. I did not run cargo or change any files.

## 0. Window, opening, search

- **Window:** 780×520, minimum 700×440. App id `okilum-settings`, window title "Okilum Settings" (`reader_settings.rs:127`).
  - On Linux and Windows the title bar reads "Settings" with a "Close" button (`:2409`).
  - On macOS a custom caption reads "Settings" with the same Close button (`reader_settings/caption.rs:79`).
  - Esc, `secondary-w` or Close all close it (`:26-28`, `:2304`).
  - There is one Settings window per owner Reader window, and it is reused.
- **Search:** there is no search field in the sidebar or across Settings. The only search box is inside **Keyboard Shortcuts**: "Search actions, contexts or shortcuts" (`reader_keymap/panel.rs:35`).
- **Starting section:** Settings reopens on the last section chosen in the sidebar (`settings_section` in reader-ui.json). With nothing saved, it opens on **About** (`:564-578`). Opening from an entry point that names a section does not overwrite the saved choice (`:767`).
- **How to open it:**
  - **Shortcut:** `secondary-,` (Cmd+, / Ctrl+,), action `OpenSettings` (`reader_settings.rs:27`). It is listed in the shortcut sheet as "Settings" (`reader_shortcuts.rs:235`).
  - **More (⋯) menu → "Settings…"** (`main.rs:7625`).
  - **App menu "Okilum" → "Settings…"** (`updater/mod.rs:40`, macOS `:50`). On macOS this menu also has "About Okilum", "Check for Updates…" and "Receive Beta Builds" (checked).
  - **About:** More menu → "About Okilum" (`desktop_app_menu.rs:17`) and macOS "About Okilum" both open Settings on the About section.
  - **Shortcuts into a specific section:** see §3.

## 1. Sidebar sections, in order (`Section::NAVIGATION`, `reader_settings.rs:526`)

| # | Label | Icon | When it appears |
|---|---|---|---|
| 1 | "Features" | Settings | Always |
| 2 | "Appearance" | Palette | Always |
| 3 | "Editor" | FileText | Always |
| 4 | "Keyboard Shortcuts" | Settings | Always |
| 5 | "Vaults" | Folder | Always |
| 6 | "T3" | SquareTerminal | Only when the **Agents** feature is on (`available()`, `:542`). The sidebar filters it out otherwise; a forced entry falls back to Features. |
| 7 | "Updates" | arrow-down-circle | Always. The content changes by platform (§2.8). |
| 8 | "Inbox" | Inbox | Always (placeholder) |
| 9 | "Sync" | RotateCw | `#[cfg(linux/macos/windows)]`, so every desktop build. It shows **even when the Sync feature is off**: clicking it turns the Sync feature on (`enable_section`, `:729`). If that fails, Settings falls back to Features. |
| 10 | "Logs" | SquareTerminal | Always ("developer options sit low") |
| 11 | "About" | Info | Always, last |

- The `brain` feature does not add a section. It only enables the "Second brain" switch under Features.
- `settings-ui-harness` and `appimage-acceptance-harness` are test-only hooks and add no sections.

## 2. Contents of each section (in order)

### 2.1 Features (`:1542`, module definitions in `reader_features.rs`)

The first line is "Keep the editor fast. Enable optional modules when you need them." Then one switch per module from `Feature::ALL`. The flags are saved in `<state>/features.json` (keys `agents`, `second_brain`, `sync`), not in reader-ui.json.

| Label | Help text | Control | Condition |
|---|---|---|---|
| "Agents" | "Projects, Board, Manager and T3. Set up a connection when you first use it." | Switch | Always available |
| "Second brain" | "Open a managed workspace. Your notes remain available in the editor." | Switch | Usable only with `cfg(all(unix, feature="brain"))`. Elsewhere it is disabled and reads "This module is not available on this platform yet." |
| "Sync" | "Keep folders in sync. Choose and connect a folder when you first use it." | Switch | linux/macos/windows |

If the preferences file can't be read, a red error line appears and the switches can only be turned off (`:1550`, `:1554`).

### 2.2 Appearance (`:1557`)

| Label | Control | What it changes | Line |
|---|---|---|---|
| "Mode" ("Light, dark, or automatic.") | Button group: "System" / "Light" / "Dark" | `appearance` | `:1565` |
| "Open another vault" ("Choose where another vault opens. New Window always creates a window.") | 3 buttons: "In this window" / "In a new window" / "Ask every time" | `vault_open` = current / new / ask | `:1606` |
| "Text size" ("Reading text across all windows.") | Stepper −/+ showing e.g. "16.0", range 12–24 | `font_size` | `reader_reading_controls.rs:10` |
| "Normal note width" ("Width of the note column when full note width is off.") | Slider 560–1200 px in 10 px steps, shows "N px" | `reading_width` | `reader_reading_controls.rs:55` |
| "File tree labels" ("Show file names as on disk, or note titles. Sorting follows the chosen label.") | 2 buttons: "File name" / "Title" | `tree_note_titles` | `:1622` |
| "Show the open note in the file tree" | Switch | `tree_auto_reveal` | `:1637` |
| "Show labels on toolbar buttons" ("Show names beside icons when there is room.") | Switch | `toolbar_labels` | `:1652` |
| "UI zoom step" ("How much Cmd/Ctrl+Shift+wheel and Cmd/Ctrl+Shift+plus or minus change the size of the whole interface at a time.") | 3 buttons: "2%" / "5%" / "10%", default 5% | `ui_zoom_step` | `ui_zoom_step_setting` |
| Heading "Color theme" | Clickable swatch cards: "Okilum", "Graphite", "Paper", "High contrast", "Nord" | `theme` | `:1667`, `theme_picker.rs` |
| Heading "Document fonts" | Reset button with tooltip "Default fonts: Noto Sans and Cascadia Code" | `note_font` + `code_font` | `reader_fonts.rs:149` |
| ↳ "Note text" ("Reader, Live Preview and tables.") | Searchable font dropdown | `note_font` | `reader_fonts.rs:172` |
| ↳ "Code" ("Source, code files and code spans.") | Searchable font dropdown | `code_font` | same |
| Preview lines | Read-only text: "The quick brown fox · אבג · 😀" and "let answer = 42; // 😀" | — | `reader_fonts.rs:194` |

### 2.3 Editor (`:1677`)

| Label | Control | What it changes | Line |
|---|---|---|---|
| "Open notes in" | 2 buttons: "Read" / "Edit" | `note_open_mode` | `:2463` |
| "Convert images to WebP" | Switch | `image_import.enabled` (default on) | `:360` |
| "Image quality" | Stepper −/+ in 5 % steps, 1–100, shows "85%" | `image_import.quality` | `:368` |
| "Keep original images" | Switch, disabled while WebP is off | `image_import.keep_original` | `:369` |
| "Ask before copying files larger than" | Stepper −/+ through 10/25/50/100/250/500 MB, 1/2 GB, "Never ask" | `paste_ask_above_mb` (default 100) | `:1680` |
| "Upload pasted and dropped files to pomoi.co" | Switch | `upload_pasted_files` | `:1362` |
| "pomoi.co token" (help text depends on state) | Masked text field ("Paste a pomoi.co token") + "Save"; "Remove" appears once a token is saved | OS credential store, not a file | `:1373` |
| "Close brackets and quotes" | Switch | `auto_pair_brackets` | `:1687` |
| "Close Markdown marks" | Switch | `auto_pair_markdown` | `:1703` |
| "Close code fences" | Switch | `close_code_fences` | `:1719` |

### 2.4 Keyboard Shortcuts (`reader_keymap/panel.rs:394`)

The file behind this section is `<config>/okilum/keymap.json` (`reader_keymap.rs:351`).

- Intro text: "Changes apply on this computer. keymap.json reloads automatically; all other defaults remain available."
- Search field: "Search actions, contexts or shortcuts".
- Buttons: "Open keymap file", "Reload", "Reset all".
- Count line: "N actions / contexts".
- A virtual list, 460 px tall. Each row has:
  - the action name;
  - "context · action";
  - "Modified" or "Default";
  - one "‹key› ×" button per bound key (tooltip "Disable this shortcut");
  - "Record" and "Reset" buttons.
- Recording panel:
  - scope buttons "Global" / "ReadMode" / "Editor";
  - "Press a shortcut…";
  - conflict warning "Already assigned to …";
  - "Assign shortcut" (or "Reassign shortcut") and "Cancel (Esc)".

### 2.5 Vaults (`:1730`, list view in `reader_settings_vaults.rs`)

**Card A: the current vault** (accent border; shown only when a vault window owns Settings). Without a vault, the card is replaced by "Open a vault to change its name, colour and files."

1. **Header row:**
   - colour dot (if a colour is set);
   - vault name with a "Current" badge, plus a "Default" badge when it is the default;
   - short path, e.g. `~/…/name` (full path in a tooltip);
   - "Rename…" button, which writes `<vault>/.okilum/vault.json` (`vault_name.rs:22`);
   - Reveal icon button, tooltip "Reveal in Finder" / "Reveal in Explorer" / "Reveal in File Manager" (`:1792-1819`).
2. **"Last opened: YYYY-MM-DD HH:MM TZ"** with a "More…" dropdown: "Open in new window", "Make default" or "Clear default", "Remove from list" (`reader_settings_vaults.rs:124`).
3. **"Now sidebar"** ("Show project activity alongside the file tree in this vault."). Switch. Saved per vault in `<state>/sidebar/<key>.json` (`now_enabled`) (`:1824`).
4. **"Show hidden files"** ("Include hidden files in this vault’s sidebar."). Switch, same per-vault sidebar file (`show_hidden`) (`:1839`).
5. **"Syncthing"** row. It appears only if the vault is a Syncthing folder. Help text depends on the `.stignore` state; buttons "Add to .stignore" and "Offer again" (`reader_stignore.rs:164`).
6. **"Vault colour"** ("A dot by the name and a line along the top."). Swatches: None, Red, Orange, Amber, Green, Teal, Blue, Purple, Pink, plus the custom colour if one is set. Saved in `vault_colors[root]` (`:1857`).
7. **"Custom colour"** (`:1107`):
   - "Current accent" chip;
   - "Choose colour" picker;
   - `#RRGGBB` text field;
   - "Apply" (or Enter) and "Cancel".
8. **"Tint title bar"**. Switch, shown only when a colour is set. Saved in `untinted_vaults` (`:1864`).
9. **"Templates folder"** ("Templates for new notes."). Read-only folder name and a folder-open button. Saved in `<state>/templates/<key>` (`:1474`). Requires `unix` or `windows`.
10. **Daily notes** (`reader_daily_notes.rs`, unix and windows, `reader_settings.rs:1888`). Three rows, saved in `<state>/daily-notes/<key>.json`, never inside the vault. Until Okilum has a saved choice, the rows follow `.obsidian/daily-notes.json` and the folder help says so.
    - **"Daily notes folder"** ("Where Today's note goes."). Shows "Vault root" or the folder path, and a folder button ("Change daily notes folder"). A folder outside the vault is refused.
    - **"Daily note name"** ("A / makes a folder, such as one per year and month."). Dropdown of `YYYY-MM-DD`, `YYYY/MM/YYYY-MM-DD`, `YYYY/MM/DD/YYYY-MM-DD`, `DD.MM.YYYY`, plus a format read from Obsidian that is not in that list. The button label is today's date in the current format.
    - **"Daily note template"** ("Applied when Today's note is created. None: just the date as its title."). Shows "None" or the template file stem, a clear button once a template is set, and a file button.
11. **"Reminder time"** ("Date-only reminders notify then, never between 21:00 and 09:00."). Stepper "Earlier" / "Later" around "HH:MM" (`reader_reminder_settings.rs:246`).
12. **"Reminders note"** ("Where “Remind me on…” adds tasks."). Note name and a folder-open button; shows a warning if the note is missing (`:284`).
    - Items 11–12 are saved in `<state>/reminder-settings/<key>.json`.

**Block B: "Other vaults"** (`reader_settings_vaults.rs:246`):

- Heading "Other vaults".
- "Removing a vault from this list keeps its files and last reading position."
- Muted note: "The default opens when no previous window can be restored. Explicit file and folder opens take priority."
- Buttons **"Open folder…"** (dispatches `OpenFolder`) and **"Refresh"**.
- Status lines: "Loading vaults…" or "No other vaults. Open a folder to add one."
- **One card per known vault, excluding the current one.** It is not a table. Each card stacks:
  - line 1: name in bold, the plain word "Default" if it is the default, and "Unavailable" or "Checking…";
  - line 2: the full path, not shortened;
  - line 3: "Last opened: …" or "Not recorded";
  - a wrapping row of three text buttons: **"Open in new window"** (disabled unless the folder exists), **"Make default"** or **"Clear default"**, and **"Remove from list"**.
- **Order:** the most recently opened vaults first (`recent_roots`), then the remaining remembered vaults sorted by path (BTreeMap) (`reader_history.rs:360`). Folders opened in the quick viewer are excluded.
- **What the list can't do:**
  - no drag to reorder, sort or search;
  - no confirmation before "Remove from list";
  - no undo.
  - Rename and colour work only on the current vault (card A), not on rows in this list.
  - "Default" in a row is plain text, while card A shows a badge.
  - The current vault's list actions are hidden behind "More…" in card A.
- **Where the list lives:** `<state>/update-session.json` (`reader_history.rs:57`).

### 2.6 T3 (`reader_t3_settings.rs:359`, Agents feature only)

1. Text "T3", then "Sign in once for every project in this app."
2. Text field (placeholder "t3.oklabs.uk or 10.10.0.23:23110") and a **"Sign in"** button; Enter also submits.
   - The server address is saved through `project::save_browser_environment` in app state, only after a successful connection.
   - The token goes to the OS credential store.
3. Read-only status line.
4. **"Manager · ‹stage›"** block (`cfg(native_manager)`, which `build.rs:16` sets for linux/macos/windows):
   - one sentence describing the stage;
   - primary button whose label depends on the stage: "Sign in to T3", "Connect manager", "Open T3 again", "Connecting…", "Open T3", "Retry now", "Reconnect" or "Try again";
   - "▸ Details" / "▾ Details" toggle. When open it shows the details text, "Copy details", "CLI fallback" help with "Copy command", a masked field ("Paste a T3 pair link or pairing code"), "Use pasted link", and "Cancel" while waiting (`:215`).
5. While sign-in is pending:
   - field "Paste the browser callback URL here";
   - buttons "Use callback URL", "Wait longer", an open-in-browser icon, and "Cancel".
6. On error: red error text and a "Details" toggle.

### 2.7 Inbox (`:2130`)

- "Not connected".
- "Inbox brings in notes you send from your phone or other apps. Connecting to it will come later."
- No controls.

### 2.8 Updates (`:1952`)

- Read-only line: "Version X · Build N · Channel: Beta|Stable|…".
- **When the updater is available** (macOS Sparkle install, installed Windows build via Velopack, or a Linux AppImage; `updater/mod.rs:280`):
  - Channel button group "Stable" / "Beta". It is saved by the platform: Sparkle on macOS, `<state>/windows-update-channel` on Windows, the AppImage channel file on Linux. The buttons are disabled while an update is running.
  - Icon button whose label comes from `action_label()`: "Check for Updates…", or "Restart now" / "Restart to update" once an update is ready.
  - Three mode buttons: "Download in the background, install on click" / "Ask before downloading" / "Off · manual check only". Saved in `<state>/update-mode`.
  - "Downloaded updates are installed only when you choose Restart now."
  - Status row, with "Restart now", "Cancel update" and "Download update" where they apply.
  - A sentence describing the chosen channel.
- **When it isn't available:**
  - Linux: "Updates are managed by your system package manager." plus a "Repository setup" icon that opens the docs.
  - Windows: "Download a new Windows ZIP from Releases to update."
  - macOS dev build: "Automatic updates are available in the installed release build."
  - If the update helper needs repair, a repair message and a **"Retry now"** button appear.
  - A "Release notes" icon is always shown.

### 2.9 Sync (`reader_settings_sync/view.rs`)

On a **portable Windows build** the section shows only: "Sync is available in the installed version", an explanation, and "Download the installer" (`:471`).

**Main view:**

1. **Approval banner** (only for an incoming request): "‹name› wants to connect", a device line, a check sentence, and **"Reject"** / **"Approve"** (`:732`).
2. **Header:** "Sync" with "Keep your vaults together across computers." and a primary **"Add computer"** button, which opens the wizard (`:54-91`).
3. **Status line:**
   - summary text, e.g. "Connect your first computer to start Sync" or "Your local vaults are up to date". While a check is in the way, the page can also say "Starting first sync as soon as the current Sync check finishes…" or "Sync is finishing the previous step. Start first sync becomes available when it is done.";
   - **"Check again"**, or on error **"Try again"** / "Create new link" / "Review existing setup" / "Choose connection";
   - "› Details" (technical details), any history or policy warnings, and "Checking…" (`:528`).
4. **First-sync recovery** (if one is pending): "First Sync needs attention: ‹path›" with **"Recover first sync"** and **"Cancel before merge"** (`first_sync.rs:261`).
5. **No computers paired yet:** card "Your notes, on your other computers" with **"I already have a pairing link"**.
   **Otherwise:** "Computers" with "This computer: ‹name›". One card per paired computer: name, OS and vault details, "Online" / "Offline" / "Connection not checked", "Last seen …", and buttons **"Remove"** and **"Vaults →"** (`:108-238`).
6. **Introduced computers** (not paired directly): name, "‹device id› · Online|Connected through your shared vaults", and **"Remove"**. If removals are still being delivered, a note says so.
7. **"Vaults"** heading ("Local vaults" when nothing is paired). One card per shared vault: label, path, "Shared with …" or "Not shared with another computer yet", status, "N files · M MB", and buttons **"Shared exclusions"** (opens a sheet, "Shared Sync exclusions") and **"Manage →"** (opens a dialog showing label, path, connection type, "Shared with" and state).
8. **Local vaults not yet shared:** name, path, "Only on this computer", and a button **"Share with my computers"** (or "Shared with approved computers") (`advanced.rs:180`).
9. **"Previous Sync versions · ‹path›"** per vault: "Keep for N days…" with buttons **"7 days" / "30 days" / "90 days" / "365 days"** (`:408`).
10. **"› Advanced · Network, existing Syncthing & conflicts"** toggle (`:423`).

**Advanced panel** (`advanced.rs:3`):

- "Help: connect a computer" (opens a sheet, "Sync setup help").
- Linux only, **"Sync connection"**: one row per detected Syncthing instance (device id, vault count) with a **"Use connection"** / "Selected" button.
- Linux only, **"Connections on this computer"**: a network description and a switch **"Only local network and VPN"** (only when Okilum manages Syncthing; otherwise an explanatory note).
- **"Servers and other Syncthing apps"**: two read-only paragraphs.
- **"Already syncing this vault with Syncthing?"** with **"Choose existing synced vault"**. This leads to a consent step with **"Use this connection"** / "Cancel".
- Linux only, **"Conflicts"**: a conflict list per vault (`reader_sync_conflicts::InventoryView`).

**Wizard dialog** (560 px wide, `:806`). Steps:

| Step | Title | Contents |
|---|---|---|
| Start | — | Field "This computer’s name", **"Create pairing link"**, "I have a link from another computer" |
| Paste | — | Name field, "Pairing link" field, **"Request connection"** |
| Link | — | "One computer · valid for one hour", the URL, **"Copy link"**, "Cancel invitation" |
| Waiting | — | "Still waiting · no files are shared before approval", "Cancel request" |
| Vaults | — | "Available vaults", per vault **"Choose folder"**, "Selected: …", **"Start first sync"** |
| Merge | — | Preview, "Back to folders" |
| Computer | — | Shared vaults, **"Choose vaults to receive"** |
| Progress | — | Progress per vault, "Choose more vaults" |
| Expired | — | **"Create new link"** |
| Existing | — | Consent step from the Advanced panel |

In every other Settings section, a Sync health or approval banner is pinned at the bottom (`reader_settings.rs:2454`).

### 2.10 Logs (`:1902`)

1. "For log files of software you build: where file:line links open."
2. **"Open log source links in"**. Dropdown: "VS Code" / "Cursor" / "Zed" / "CLion / JetBrains" / "Custom command". Saved in `log_editor` (`:1065`).
3. **"Editor command"**. Text field ("code --goto {file}:{line}:{col}"), shown only when Custom is chosen. Saved in `log_editor_command` (`:1092`).
4. Explanatory text about saved regexes, then one row per saved regex: name and a **"Delete"** button. Saved in `log_regex`.
5. Heading **"Log sources · ‹vault›"** with "Stored with this vault. Switch vault to edit another vault's sources." (or "Open a vault to edit its log sources.").
6. Buttons **"HTTPS log files…"** and **"Kibana sources…"**, disabled without a vault. Then "Import Discover URLs for the current vault…".
7. **"Log source paths"**. Field ("Build path, e.g. /src/ordgw") with a folder button; then rows "from → to" with a remove (×) button. Saved per vault in `source_maps[root]` (`:1240`).

### 2.11 About (`about.rs:99`)

- Logo, "Okilum", "Your notes. Clearly connected.", a description and 4 bullet points.
- "Version … · Build … · ‹channel› · ‹platform›" with **"Copy"** (changes to "Copied"), plus a check-for-updates icon when the updater is available.
- Update status row.
- macOS: **"Uninstall Okilum…"**.
- Link buttons: "Repository", "Release notes", "MIT License", "Third-party notices", "What's new in this build".
- Linux: where updates come from (pacman repo or AppImage).
- Windows without updater: "To update, download the latest Windows ZIP." and "Okilum links open in the installed version."

## 3. Settings also reachable from somewhere else

| Setting | In Settings | Also reachable from |
|---|---|---|
| Open Settings | — | `secondary-,`; More → "Settings…" (`main.rs:7625`); app menu "Settings…" (`updater/mod.rs:40/50`); shortcut sheet "Settings" |
| Mode (System/Light/Dark) | Appearance | More menu → "Appearance" label → "System" / "Light" / "Dark", checked items (`main.rs:7685-7688`) |
| Show hidden files | Vaults card | More → "Show hidden files" (checked, `main.rs:7663`); sidebar header icon `sidebar-show-hidden` and its overflow in the vault-name menu (`main.rs:7415`, `reader_vaults.rs:262`); breadcrumb tooltip (`reader_tree_breadcrumbs.rs:262`); shortcut `ToggleHiddenFiles` (Ctrl+Shift+. / ⇧⌘., `main.rs:900-903`); shortcut sheet |
| Now sidebar | Vaults card | `ToggleLeftMode` "Now / Tree" turns Now on when you switch to it (`reader_workbench.rs:217`): View menu "Now / Tree" (`main.rs:560`), Ctrl+Shift+\ / Ctrl+\| (`main.rs:861`), sidebar toggle; the "Use Now" offer button (`reader_workbench.rs:316`) |
| Open another vault | Appearance | The "Where should … open?" dialog: "In this window" / "In a new window" with a "Remember my choice" checkbox (`reader_open.rs:1430-1521`) |
| Text size | Appearance | Indirectly only: the `ZoomIn` / `ZoomOut` / `Reset` zoom overlay ("−  110%  +  Reset") and View menu "Zoom In/Out/Actual Size" change `text_zoom`, which is a separate multiplier, not `font_size` |
| Remove from list | Vaults list + current-vault "More…" | The vault-name menu → "Known vaults" → "Remove from list" (`reader_vaults.rs:208`) |
| Open folder… | Vaults list | More → "Open folder…"; vault-name menu "Open folder…"; `OpenFolder` shortcut |
| Open in new window | Vaults | Choosing a vault in the vault-name menu (follows `vault_open`) |
| Open Vaults section directly | — | Vault-name menu → "Vault settings" (`reader_vaults.rs:250`); Now-sidebar menu → "Vault settings" (`reader_workbench.rs:657`) |
| Today's note | Vaults → daily notes folder, name and template | More → "Today's note" (`main.rs:7650`); sidebar header button `sidebar-daily-note` (unix/windows); Now menu → "Today's note" (`reader_workbench.rs:634`) |
| Add to .stignore | Vaults card | The offer shown when the vault opens (`reader_stignore.rs:84`) |
| Agents feature | Features | The Agents offer → "enable" (`reader_agents_offer.rs:373`) |
| Open Features section | — | "Settings → Features" buttons when Second brain is off (`brain.rs:4214`, `workspace.rs:913`) |
| Open T3 section | — | Project Manager sign-in actions (`project.rs:1365`, `project/native.rs:235`, `project/native/onboarding.rs:105`, `reader_t3_settings.rs:59`) |
| Open Sync section / start pairing | — | More → "Share with another computer…" (`reader_settings.rs:76`, `main.rs:7636`); start screen "Get notes from another computer" (opens pairing and turns Sync on, `reader_startup.rs:607`); Sync health banner "Fix" (`reader_settings_sync/health.rs:97`) |
| Kibana sources… | Logs | App section Logs (More → "Logs") → "Kibana sources…" (`reader_project_navigation.rs:1425`) |
| Saved regexes | Logs (delete only) | Created and applied from the "Regex" button in a log file (`reader_log.rs:3721`) |
| Check for updates | Updates | About icon (`about.rs:179`); macOS "Check for Updates…"; More menu on Windows (`desktop_app_menu.rs:28`); ready toast |
| Beta channel | Updates | macOS app menu "Receive Beta Builds" (`updater/mod.rs:37`) |
| About / Copy version | About | More → "About Okilum", "Copy version info" (`desktop_app_menu.rs:17,22`); Help → "Copy Version Info" (`version_info.rs:21`); macOS "About Okilum" |
| Keyboard shortcuts | Keyboard Shortcuts (editable) | More → "Keyboard shortcuts", Cmd+/ / Ctrl+/ and the empty-state button open the shortcut sheet, which is read-only and does not link to Settings (`main.rs:733`, `reader_document_menu.rs:775`) |

**Settings-owned controls** (some have contextual entry points described above): theme cards, fonts, note width slider, file tree labels, open-the-note-in-tree, toolbar labels, open notes in, the three image options, paste threshold, upload switch and token, bracket/Markdown/fence closing, vault rename, colour and tint, templates folder, daily notes folder, name and template, reminders, update mode, log editor, source paths, HTTPS sources, and the whole Sync management area.

**Preferences that have no Settings row:**
- **Only toggled from menus, toolbar or shortcuts:** "Wrap long lines" (`code_soft_wrap`: More menu and `ToggleSoftWrap`), "Full Note Width" (View menu, document ⋯ menu, Ctrl+Alt+W), "Focus Mode" and "Typewriter Mode" (View menu and shortcuts), text zoom.
- **No UI at all:** `emoji_completion` is saved (default on), but nothing outside tests ever changes it.
