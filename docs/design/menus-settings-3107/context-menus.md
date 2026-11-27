# Reader More and context-menu inventory

Baseline: `ebc805082bfe52c91c6516cf36903d54cd5cf69a` (20 November 2026).
Source inspection only. Unless explicitly qualified, an entry is shared by macOS,
Windows and Linux desktop builds. Lists below retain builder order; separators
are written as `|`. A dynamic family means one entry per live value, not one
literal item. Shared families are expanded once and referenced by name.

## Reader More

Source: [reader_more_menu](../../../crates/okilum-shell/src/main.rs#L7557),
[platform tail](../../../crates/okilum-shell/src/desktop_app_menu.rs).

1. Reader; Inbox; Projects (Agents enabled); Logs; Second brain… (Unix + brain
   build + enabled Second brain module).
2. `|` Settings…; Project service storage…; Share with another computer…;
   Keyboard shortcuts (read-only sheet); Maximize note.
3. `|` New note…; New untitled file; New Folder; New note from template…;
   Today's note; Quick capture…; Recover notes…; Recover link moves….
4. `|` Recent files…; Open file…; Open folder…; Switch to single file mode
   OR Open folder as vault (window mode); New Window.
5. `|` Show hidden files (checked); Wrap long lines (checked).
6. `|` Collapse all folders. `|` Appearance label; System; Light; Dark (checked).
7. `|` About Okilum; Copy version info; platform update entry; Quit Okilum.

Share opens Settings → Sync. The update entry is absent on macOS; Windows shows
the updater's current action (or "Windows updates are not available yet — download
the new ZIP"); Linux says "Updates come with system updates". Those labels are
current source wording, not a proposed update contract. All three OSs have this
menu. It is not generated from the native app menu.

## Shared file actions

Source: [reader_files](../../../crates/okilum-shell/src/reader_files.rs#L197),
[Open in](../../../crates/okilum-shell-platform/src/open_in.rs#L313),
[OS labels](../../../crates/okilum-shell-platform/src/platform/labels.rs).

**F:** Open in → one entry per installed supported editor (absent when none),
Reveal in Finder / Show in Explorer / Show in File Manager;
Open in terminal; Copy absolute path; Copy vault path; Copy wikilink;
Copy Okilum link; Open with default app; Quick Look (macOS only).
Installed editor discovery is dynamic and platform-specific; its label list is
defined in `open_in.rs`, not an additional static application-menu category.
Trash wording is Move to Trash (Mac/Linux) / Move to Recycle Bin (Windows).

## Navigation and documents

| Surface / source | Entries, gates and target |
| --- | --- |
| Tree file or directory: [reader_item_menu](../../../crates/okilum-shell/src/main.rs#L13764) | F; Rename… and Move to… for movable non-attachment entries; Pin/Unpin when eligible; `|` OS Trash. Targets the clicked path. |
| Tree multi-selection: same builder | Move selected…; Rename selected…; Trash selected… replace single-item actions when the clicked row belongs to the selection. |
| Tree file additions: [tree builder](../../../crates/okilum-shell/src/main.rs#L5641) | Add to log view when it is a log file not already in the open log view. |
| Tree directory additions: same builder | Make project… (Agents enabled and eligible folder); New File; New Folder; `|` Expand all subfolders; Collapse all subfolders. Creation is Unix/Windows. |
| Empty tree: [root menu](../../../crates/okilum-shell/src/main.rs#L5000) | New note in vault root; New folder in vault root. |
| Vault name: [reader_vaults](../../../crates/okilum-shell/src/reader_vaults.rs#L124) | Known vaults label; dynamic vault names with checking/unavailable suffix and current check; Remove from list beside each; `|` Open folder…; OS Reveal vault; Vault settings (not single-file); overflowing sidebar actions. Loading/error/empty states are labels. |
| Sidebar header: [SidebarAction](../../../crates/okilum-shell/src/main.rs#L7364) | Fixed order on unix/windows: New note; Today's note; New folder; Collapse all folders; Show hidden files (checked). Only Show hidden files moves into the header ⋯ (`main.rs:4935`) when the row is narrow. Search is not one of these buttons. While Now/Tree is drawn in the header, the same actions move into the vault-name menu instead. |
| Now actions: [reader_workbench](../../../crates/okilum-shell/src/reader_workbench.rs#L627) | New note; Today's note; Reveal current note in Tree; Collapse sections; Vault settings. |
| Now note title: [breadcrumbs](../../../crates/okilum-shell/src/reader_tree_breadcrumbs.rs#L213) | OS Reveal; Pin/Unpin for the represented note. Breadcrumb overflow dropdowns contain dynamic ancestor/earlier-root folder names. |
| Pinned row: [reader_pin_row](../../../crates/okilum-shell/src/reader_pin_row.rs#L254) | Unpin; `|` F while target exists. Missing target: Remove pin only. |
| Note/document More: [reader_document_menu](../../../crates/okilum-shell/src/reader_document_menu.rs#L377) | Reveal in sidebar; `|` Full note width/Normal note width when not inline; Zoom In; Zoom Out; Actual Size when width applies; Zoom In UI; Zoom Out UI; Actual UI Size; `|` Find in note when not inline; Save for dirty editor when not inline; Live Preview; Focus mode and Typewriter mode while editing; Source for Live Preview when not inline; Rename; Move to…; Note history; `|` OS Reveal; Open in terminal; Copy path (absolute); Copy Okilum link; Open in → editors; Open in new window; Pin/Unpin; `|` OS Trash; `|` Close note. Presentation/Save/Find items depend on responsive toolbar visibility. |
| File through the same document builder | Relevant reveal/width/zoom/find subset; Copy vault path; Copy wikilink; Open in → editors; eligible pin; OS Trash; Close file. New viewer chrome below has its own responsive builder. |
| New note from template: [reader_templates](../../../crates/okilum-shell/src/reader_templates.rs#L159) | Dynamic template file stems; click creates from the chosen template. |
| Typed-note view picker: [tree builder](../../../crates/okilum-shell/src/main.rs#L5291) | Standard note and each available typed view label. |

## Read, editor and embedded objects

| Surface / source | Entries and conditions |
| --- | --- |
| Read text: [commands](../../../crates/okilum-shell/src/reader_read_menu/commands.rs) | With selection: Copy; Copy as Markdown; Copy as Rich Text. Background: Select all; Copy entire document as Markdown. Rich Text disabled for HTML rendering. Object menus append selection commands only when a selection exists. |
| Read code / heading: [context hook](../../../crates/okilum-shell/src/reader_context_menu.rs#L75), [heading link](../../../crates/okilum-shell/src/reader_deep_link.rs#L12) | Read commands plus Copy code for the clicked code block; Copy link to heading on a heading. Selected parseable date adds Remind me on ‹date› through reader_reminder. |
| Link: [reader_files](../../../crates/okilum-shell/src/reader_files.rs#L391) | Copy link address; Copy link text; F for a resolved in-vault target; selected-text copy formats. Unresolved targets keep address/text actions without acquiring file actions. |
| Existing outside-vault link: [outside_file_menu](../../../crates/okilum-shell/src/reader_files.rs#L341) | Open with default app; Reveal ‹filename›; Copy absolute path. |
| Editor: [reader_reminder](../../../crates/okilum-shell/src/reader_reminder.rs#L118), [context_menu](../../../crates/okilum-shell/src/reader_editor/context_menu.rs) | Go to Definition and Show Code Actions when supported; Cut; Copy; Copy as Markdown for Markdown; Paste; Paste as Plain Text; Paste as Titled Link; Select All; Copy link to line; Open block in ‹editor› for a code block; Stop external editing when active; Remind me on ‹date› for a selected date. Edit/clipboard availability disables actions appropriately. |
| Markdown editor Format submenu | Same Bold, Italic, Strikethrough, Inline Code, Quote, Heading 1–6, Paragraph, Format code, Validate code, Repair code… as [menu bar](menu-bar.md). Only editable Markdown gets this submenu. |
| Markdown table in editor: [tables](../../../crates/okilum-shell/src/reader_editor/tables.rs#L49) | Table → Edit table as grid; `|` Insert row below; Delete row; Insert column after; Delete column; `|` Align column left/center/right; `|` Format table. The grid uses the same table actions. |
| Rendered table: [reader_table_copy](../../../crates/okilum-shell/src/reader_table_copy.rs#L91) | Copy table as Markdown; Copy table as TSV; selection copy formats if any. |
| Image/attachment, Read and editor: [reader_image_actions](../../../crates/okilum-shell/src/reader_image_actions.rs#L123) | Open image in default app / Open in default app; Show in sidebar; Copy path (vault-relative here); Copy absolute path for local/unavailable targets; images add Copy image; Copy image address; Save image as… on the accepted Read image surface. iCloud placeholder adds Download from iCloud, with disabled unavailable actions and explanatory notice. Remote images lack local-path actions. Source/Live Preview append their image actions to the editor menu. |
| Code text: [reader_code_file](../../../crates/okilum-shell/src/reader_code_file.rs#L732) | Editable: Cut, Copy, Paste; read-only: Copy; `|` Select All. Structured node source adds Copy node. |
| Structured file tree: [structure](../../../crates/okilum-shell/src/reader_code_file/structure.rs#L669) | Copy node; Show in raw text; Fold all; Unfold all. |
| Managed Brain source: [source_input](../../../crates/okilum-shell-brain/src/brain/source_input.rs) | Uses the toolkit text menu / read-only editor capabilities, not another custom product command list. Unix brain builds only. |

## File viewers and specialized menus

| Surface / source | Entries and gates |
| --- | --- |
| Viewer More: [reader_viewer_chrome](../../../crates/okilum-shell/src/reader_viewer_chrome.rs#L1519) | Reveal in sidebar; hidden width and Maximize / restore actions; overflow actions; Copy → Copy file content (text only), Copy path, Copy vault path, Copy wikilink; Open with → installed editors, default app and macOS Quick Look; Wrap long lines, Zoom in/out, Actual size for text; OS Trash; Close file. The responsive plan moves existing toolbar actions into overflow rather than adding a second implementation. |
| Specialized viewer menus: [same chrome](../../../crates/okilum-shell/src/reader_viewer_chrome.rs#L1750) | Overflow and open actions supplied by the viewer, plus Copy submenu supplied by that viewer. View dropdown choices are its existing named view tabs, e.g. Source/Table/Structure/Overview; capability-dependent, not universal commands. |
| Drawing: [reader_drawing](../../../crates/okilum-shell/src/reader_drawing.rs#L435) | Expand drawing; Edit drawing (opens Okilum's offline editor for this file; shortcut sheet lists it under View); Copy scene; Drawing details. Inline note drawings show Expand and Edit as buttons rather than a menu. |
| Tasks date: [reader_tasks](../../../crates/okilum-shell/src/reader_tasks.rs#L933) | Due today; Due tomorrow; Snooze until tomorrow; Snooze until… → date picker; Choose due date → date picker. Requires writable task. |
| HAR filters/request: [reader_har](../../../crates/okilum-shell/src/reader_har.rs#L834) | Status: All statuses + status classes; Method: All methods + methods in capture; Type: All types + Document, Fetch/XHR, Script, Stylesheet, Image, Font, Media, WebSocket, Other. Request context: Copy URL; platform cURL shell formats; Copy response body; Save response body as…. |
| HAR detail: [detail commands](../../../crates/okilum-shell/src/reader_har.rs#L1945) | Copy body / Copy all ‹section›; Save body as… for body sections; Copy value; Copy name; Copy name: value for pairs; Copy text for text. |
| HAR overview: [overview](../../../crates/okilum-shell/src/reader_har/overview.rs#L457) | Overview-supported copy formats, row Copy value/name/name: value, Copy overview. |
| Project navigation item: [reader_project_navigation](../../../crates/okilum-shell/src/reader_project_navigation.rs#L702) | Change icon…; Connect / reconnect existing local T3 project…. Agents module. |
| Manager options: [project](../../../crates/okilum-shell/src/project.rs#L1263) | Open in T3 when it overflows; Rotation policy… on native Manager; Details; Open terminal; Executors; Request executor; Renew connection… / Change connection…; Sign out. Availability follows connection state. |
| Manager narrow section picker: [chat_ui](../../../crates/okilum-shell/src/project/chat_ui.rs#L39) | Chat; Rules; Skills, with current section checked. |
| Board filters: [filters](../../../crates/okilum-shell/src/project/board/filters/ui.rs) | One entry per project label / executor, with selected checks. |
| Board related-note picker: [manage](../../../crates/okilum-shell/src/project/board/manage.rs#L524) | None (optional); dynamic note labels. |

## Log menus

All desktop platforms; source: [log chrome](../../../crates/okilum-shell/src/reader_log/chrome.rs),
[cell context](../../../crates/okilum-shell/src/reader_log/filters.rs#L596).

| Menu | Ordered entries / dynamic expansion |
| --- | --- |
| Saved views | Each saved name OR disabled No saved views yet; Save current view…; Delete → each saved name. |
| Export | Export ‹count› shown as raw lines… / JSON lines… / CSV…; same three for selected when more than one selected, and marked when nonempty; Raw values in CSV (checked). Zero-count exports disabled. |
| Files | Each file name (checked when enabled); Add file…. |
| Columns | Each field name (checked); Choose message field / Message: ‹field› → Default message field; each string field. |
| Level | All levels; Debug and above; Info and above; Warning and above; Error and above; Fatal only; Include lines without a level (checked). |
| Time | All time; Last 5 minutes; Last hour; Last 24 hours. |
| Query history | Each stored query string. |
| Cell | Include this value; Exclude this value; Copy value; Show as a column (where eligible); Mark row/Remove mark; Next mark; Previous mark; Clear all marks (disabled when empty). |
| HTTPS saved sources: [reader_log_http](../../../crates/okilum-shell/src/reader_log_http.rs#L261) | Each saved source label; selecting loads configuration before explicit Start. |
| Virtual log sort: [reader_log_virtual](../../../crates/okilum-shell/src/reader_log_virtual.rs#L674) | Each sort mode from [model](../../../crates/okilum-shell/src/reader_log_virtual/model.rs); current checked. |
| Remote guided selectors: [guided](../../../crates/okilum-shell/src/reader_log_remote/guided.rs) | Hour/minute choices and dynamic source fields/values within the source/time/scope picker. These are value selectors, not global actions. |

## Settings-local menus and platform summary

Settings-local controls are inventoried in [Settings](settings.md), including the
current-vault More menu, font and log-editor dropdowns, key binding row controls,
Sync setup choices and platform update variants. Standard toolkit text fields
retain OS text-edit menus; OS-injected items are not application-defined entries.

macOS uniquely adds Quick Look, native menu bar and installed Sparkle entries.
Windows uses Explorer/Recycle Bin wording, excludes Second brain and offers
Sync setup only in the installed package. Linux uses File Manager/Trash wording,
has Linux-only Sync Advanced connection/network/conflict controls, and its More
update text differs from the AppImage-aware Settings page. File/selection menus
otherwise share their builders across the three platforms.
