# Menu bar inventory

Source baseline: `ebc805082bfe52c91c6516cf36903d54cd5cf69a` (20 November 2026). Static source inspection; native OS menus were not exercised.

The menu bar has one definition, and every platform and build flavor gets it. Only macOS draws it. On Windows and Linux, gpui stores the menus and never renders them, so users there reach these commands through shortcuts and a separate Reader "More" (…) dropdown.

## Builders and gates

- **Two `set_menus` functions, same content.**
  - `crates/okilum-shell/src/brain/app_quit.rs:35` is used when `all(unix, feature = "brain")` is true, which means the default macOS and Linux builds (`brain` is in `default`).
  - `crates/okilum-shell/src/reader_app_menu.rs:25` is used otherwise: every Windows build (the `brain` module is unix-only, `main.rs:28`) and unix builds without `brain`.
  - Both produce `[Okilum, File, Edit, Format, View, Help]`. Each defines its own `okilum::Quit` action.
- **When the menus are rebuilt:**
  - at startup (`main.rs:9533/9535`)
  - on Full Note Width toggle (`main.rs:535-539`)
  - on Focus/Typewriter toggle (`reader_editor/writing_modes.rs:230-232`)
  - on macOS beta toggle (`updater/mod.rs:403-405`)
- **No Settings → Features flag affects the menu bar.** The only conditions are `updater::macos::available()` and `reader_feedback::sprint::enabled()`.
  - `macos::available()` is true when Sparkle started. That needs `SUFeedURL` in the bundle's Info.plist and no `OKILUM_STATE_DIR` set (`updater/bridge.m:687-701`). A bare cargo binary or a QA profile gets no update items.
  - `sprint::enabled()` is macOS only, when the release version starts with "Sprint " (`reader_feedback/sprint.rs:6-8`).
- **Unused helpers:** `MenuItem::submenu` and `os_submenu` are never used, so there are no nested submenus. `reader_editor/tables.rs:49` ("Table") is a menu builder, but only the editor context menu uses it (`reader_editor.rs:3173`), not the menu bar.

**How macOS picks the shortcut it shows** (`gpui-pre-macos-0.3.3/src/platform.rs:332-348`): it takes the **first** binding added whose context is empty or matches gpui's default Workspace/Pane/Editor context. If none matches, it takes the first binding of all. Okilum's contexts (`Reader`, `Input`, `ReaderSource > Input`) never match, so the earliest binding is shown. In two places this contradicts the code comments, which assume the last binding wins:

- **Inline Code:** the comment says ⌘`, but the menu should show ⌃` (`ctrl-\`` is added first under `cfg(macos)`).
- **Zoom In:** the comment says ⌘=, but the menu should show ⌘+ (`secondary-+` is added first).

This is from reading the code only; I did not check it on a Mac.

The menu is also built before `reader_keymap::install`, so user keymap.json overrides show up only after one of the rebuilds above.

In the tables, "Mac" is the shortcut shown in the macOS menu. "Win / Linux" is the bound key, which works on those systems even though no menu shows it.

### Okilum (app menu)

| Label | Action | Mac | Win / Linux | Platforms | Condition | Source |
|---|---|---|---|---|---|---|
| About Okilum | `okilum::AboutOkilum` | — | n/a | macOS | always | `updater/mod.rs:49` |
| Settings… | `reader_settings::OpenSettings` | ⌘, | Ctrl+, | all | always | mac `updater/mod.rs:50`; other `:40` |
| — | separator | | | all | | mac `:51`; other `:41` |
| Check for Updates… | `okilum::CheckForUpdates` | — | n/a | macOS | `macos::available()` | `updater/mod.rs:55` |
| Receive Beta Builds (checkmark) | `okilum::ToggleBetaBuilds` | — | n/a | macOS | `macos::available()`; checked = `macos::beta()` | `updater/mod.rs:56` |
| — | separator | | | macOS | | `updater/mod.rs:59` |
| Quit Okilum | `okilum::Quit` (`brain::app_quit` or `reader_app_menu`) | ⌘Q | Ctrl+Q | all | always | `brain/app_quit.rs:39` / `reader_app_menu.rs:29`; key at `:30` / `:11` |

On Windows and Linux the menu is: Settings…, —, Quit Okilum (`updater/mod.rs:39-43`).

### File (`reader_open.rs:742-755`)

| Label | Action | Mac | Win / Linux | Source |
|---|---|---|---|---|
| New Untitled File | `NewUntitled` | none | none | `:747` |
| Recent Files… | `ShowRecentFiles` | none | none | `:748` |
| Open File… | `reader_open::OpenFile` | ⌘O | Ctrl+O | `:749` (key `:729`) |
| Open Folder… | `reader_open::OpenFolder` | ⇧⌘O | Ctrl+Shift+O | `:750` (key `:730`) |
| New Vault… | `reader_open::NewVault` | none | none | `:751` |
| New Window | `reader_open::NewWindow` | ⇧⌘N | Ctrl+Shift+N | `:752` (keys `:733/:735`) |

There are no gates; this menu is the same on every platform.

### Edit (`note_paste.rs:127-144`)

| Label | Action | Mac | Win / Linux | Source |
|---|---|---|---|---|
| Undo | `gpui_component::input::Undo` | ⌘Z | Ctrl+Z | `:132` |
| Redo | `input::Redo` | ⇧⌘Z | Ctrl+Y | `:133` |
| — | | | | `:134` |
| Cut | `input::Cut` | ⌘X | Ctrl+X | `:135` |
| Copy | `input::Copy` | ⌘C | Ctrl+C | `:136` |
| Paste | `okilum_edit::PasteFromMenu` | none (on purpose: ⌘V stays with the editor, and the ⌘V key monitor finds this item by the title "Paste") | none | `:137` |
| Paste as Plain Text | `note_paste::PasteAsPlainText` | ⌥⇧⌘V | Ctrl+Shift+V | `:138` (keys `main.rs:801-814`) |
| Paste as Titled Link | `note_paste::PasteAsTitledLink` | ⌥⌘V | Linux Ctrl+Alt+V; Windows Ctrl+Shift+K | `:139` (`main.rs:504`, `:816`) |
| — | | | | `:140` |
| Select All | `input::SelectAll` | ⌘A | Ctrl+A | `:141` |

The Undo/Redo/Cut/Copy/Select All keys are in gpui-component `crates/base/src/input/base/state.rs:218-264`, context `Input`. That vendor tree is not in the Okilum commit, so I read it from the vendored gpui-component tree (commit 928c3eb7).

### Format (`reader_editor/format.rs:77-101`)

Items are enabled only while a note editor has focus.

| Label | Action | Mac | Win / Linux | Source |
|---|---|---|---|---|
| Bold | `ToggleBold` | ⌘B | Ctrl+B | `:82` (key `:50`) |
| Italic | `ToggleItalic` | ⌘I | Ctrl+I | `:83` |
| Strikethrough | `ToggleStrikethrough` | ⇧⌘X | Ctrl+Shift+X | `:84` |
| Inline Code | `ToggleInlineCode` | ⌃` shown; ⌘` also bound | Ctrl+` | `:85` (keys `:60-62`) |
| Quote | `ToggleQuote` | ⌃⇧Q | Ctrl+Shift+Q | `:86` |
| — | | | | `:87` |
| Heading 1 … Heading 6 | `Heading1`…`Heading6` | ⌘1…⌘6 | Ctrl+1…6 | `:88-93` |
| Paragraph | `Paragraph` | ⌥⌘0 | Linux Ctrl+Alt+0; Windows Ctrl+Shift+T | `:94` (`main.rs:509`) |
| — | | | | `:95` |
| Format code | `reader_editor::FormatCode` | ⌥⇧F | Linux Alt+Shift+F; Windows Ctrl+Shift+G | `:96` (`main.rs:510`, `:630`; context `ReaderFileEdit`) |
| Validate code | `ValidateCode` | ⌥⇧V | Linux Alt+Shift+V; Windows Ctrl+Shift+J | `:97` |
| Repair code… | `RepairCode` | ⌥⇧R | Linux Alt+Shift+R; Windows Ctrl+Shift+R | `:98` |

### View (`main.rs:551-571`)

| Label | Action | Mac | Win / Linux | Condition | Source |
|---|---|---|---|---|---|
| Zoom In | `reader_text_zoom::ZoomIn` | ⌘+ shown (⌘= also bound) | Ctrl++ / Ctrl+= | | note text only; Shift+= is interface zoom |
| Zoom Out | `ZoomOut` | ⌘- | Ctrl+- | | |
| Actual Size | `reader_text_zoom::Reset` | ⌘0 | Ctrl+0 | | |
| Zoom Interface In | `reader_ui_zoom::ZoomIn` | ⇧⌘= | Ctrl+Shift+= | | whole UI, 10% steps, 50–200% (#2848) |
| Zoom Interface Out | `reader_ui_zoom::ZoomOut` | ⇧⌘- | Ctrl+Shift+- | | |
| Actual Interface Size | `reader_ui_zoom::Reset` | ⇧⌘0 | Ctrl+Shift+0 | checked at 100% | |
| — | | | | | `:559` |
| Now / Tree | `ToggleLeftMode` | ⌘\| (⇧⌘\\) | Ctrl+Shift+\\ (also Ctrl+\|) | | `:560` (`main.rs:860-872`) |
| Reveal in Tree | `FocusCurrentFolder` | ⇧⌘L | Ctrl+Shift+L | | `:561` |
| Maximize Note | `MaximizeNote` | ⌥⌘F | Linux Ctrl+Alt+F; Windows Ctrl+Shift+Enter | | `:562` (`main.rs:505`) |
| Full Note Width (checkmark) | `ToggleNoteWidth` | ⌥⌘W | Linux Ctrl+Alt+W; Windows Ctrl+Shift+W | checked = `full_note_width` | `:563-564` |
| — | | | | | `:565` |
| Focus Mode (checkmark) | `ToggleFocusMode` | F8 | F8 | checked = `focus_mode` | `:566-567` |
| Typewriter Mode (checkmark) | `ToggleTypewriterMode` | F9 | F9 | checked = `typewriter_mode` | `:568-569` |

### Help (`version_info.rs:18-41`)

| Label | Action | Shortcut | Platforms | Condition | Source |
|---|---|---|---|---|---|
| Send sprint report | `reader_feedback::SendSprintReport` | none | macOS | `sprint::enabled()`, inserted at index 0 | `:27-34` |
| Send feedback… | `reader_feedback::SendFeedback` | none | all | | `:20` |
| Copy Version Info | `okilum::CopyVersionInfo` | none | all | | `:21` |
| Project service storage… | `service_storage::OpenServiceStorage` | none | all | | `:22-25` |

## What users actually see

**macOS:** a native menu bar with the six menus above. macOS itself adds a few items, such as dictation and emoji in Edit, full screen in View, and the search field in Help; that is OS behaviour, not Okilum code. Without Sparkle, the update items are missing. There is no Services, Hide or Window menu.

**Windows and Linux: no menu bar.**
- gpui's `set_menus` stores the list (`gpui-pre-windows-0.3.3/src/platform.rs:746`, `gpui-pre-linux-0.3.3/src/linux/platform.rs:621`). Both platforms implement `get_menus`, and `platform/text_platform.rs` forwards it, but nothing in Okilum calls that forward to draw a bar. There is no `AppMenuBar`. macOS `set_menus` installs an `NSMenu` (`gpui-pre-macos-0.3.3/src/platform.rs:1045`).
- The only menu-like surface is the Reader "More" (…) dropdown, `reader_more_menu` at `main.rs:7557-7688`. It is rendered on every platform, macOS included (`main.rs:4465`). It is a separate list, not a copy of the app menu:
  - Reader, Inbox, Projects (only with the Agents feature), Logs, and Second brain… (unix + `brain` + SecondBrain feature)
  - — Settings…, Project service storage…, share-sync item, Keyboard shortcuts, Maximize note
  - — New note…, New untitled file, New Folder, New note from template…, Today's note, Quick capture…, Recover notes…, Recover link moves…
  - — Recent files…, Open file…, Open folder…, "Switch to single file mode" or "Open folder as vault", New Window
  - — Show hidden files, Wrap long lines — Collapse all folders — Appearance: System / Light / Dark
  - Then `desktop_app_menu::append` (`desktop_app_menu.rs:14-42`): — About Okilum, Copy version info, an update line, Quit Okilum.
  - The update line on Windows is the updater action label when `updater::available()`, otherwise "Windows updates are not available yet — download the new ZIP". On Linux it is always "Updates come with system updates". macOS has no update line.
- **Menu-bar entries with no menu equivalent on Windows/Linux:**
  - Edit and Format commands are also offered in the editor context menu (Format is Markdown-only); zoom is also in document menus. Now / Tree and Reveal in Tree have toolbar/sidebar and shortcut access.
  - Full Note Width, Focus Mode and Typewriter Mode: also in the document menu (`reader_document_menu.rs:348-430`)
  - Send feedback…: the Reader toolbar feedback button
  - New Vault…: only from the startup screen (`reader_startup.rs:590`)

## Labels in more than one menu

- **Within the menu bar: none.** No label or action appears in two menu-bar menus.
- **Same label in the menu bar and in the More menu:** Settings…, Project service storage…, New Window, Quit Okilum; on macOS also About Okilum.
- **Same command, different capitalization (menu bar vs More):**

| Menu bar | More menu |
|---|---|
| New Untitled File | New untitled file |
| Recent Files… | Recent files… |
| Open File… | Open file… |
| Open Folder… | Open folder… |
| Maximize Note | Maximize note |
| Copy Version Info | Copy version info |

- The toolbar and document menu label this toggle "Full note width" / "Normal note width" (`main.rs:542-549`), while the View menu always says "Full Note Width" with a checkmark.
