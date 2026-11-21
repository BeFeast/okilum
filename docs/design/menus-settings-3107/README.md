# Menus and Settings: inventory and design choices

Tracking: [#3107](https://git.oklabs.uk/BeFeast/okilum/issues/3107).
Scope confirmed by Oleg on 18 November 2026 in
[comment 83571](https://git.oklabs.uk/BeFeast/okilum/issues/3107#issuecomment-83571).

This is the inventory and mockup stage. It changes no application behavior.
The source baseline is main `ebc805082bfe52c91c6516cf36903d54cd5cf69a` (20 November 2026).
All platform statements below come from source inspection, not from driving
installed apps. Native OS additions and installed editor lists remain dynamic.

## Review material

- [Menu bar](menu-bar.md): every application-defined item, platform gates and shortcuts.
- [Settings](settings.md): every section, row, nested setup surface, scope and duplicate entry point.
- [More and context menus](context-menus.md): ordered entries, shared builders and dynamic choices.
- [Interactive mockups](mockups.html): three alternatives, each with command search,
  Settings search, Vaults and a contextual menu. Open locally in a browser; no server,
  account or dependencies are required. All sample vaults and values are illustrative.

There is no global menu search or Settings search in the baseline. Keyboard
Shortcuts has its own local search; log queries, Quick Open, dropdown option
searches and macOS Help search do not search all commands and settings.

## What needs a consistent home

| Command or preference | Current entry points | Proposed owning category (all alternatives) |
| --- | --- | --- |
| Settings / About / updates / version | Native app menu, Help, Reader More, About and Updates sections | Application; About and Updates in Settings → Application |
| New/open/recent/window/recovery | File, Reader More, startup, tree and document actions | File; Recovery as a secondary group |
| Today's note | More, sidebar header, Now menu | File command; folder, name and template live in Settings → Vaults |
| Copy/reveal/open externally/rename/move/trash | Tree, document, pin, link and viewer menus | Contextual File actions; operate on the clicked target, not the open note |
| Copy vs Copy as Markdown vs Copy as Rich Text | Edit, editor and Read selection menus | Edit → Copy; retain selection semantics and formats |
| System/Light/Dark | More → Appearance and Settings → Appearance | Settings → Appearance; contextual shortcut can deep-link to the same setting |
| Hidden files / Now / tree labels / auto reveal | More, sidebar overflow, View, Settings → Vaults/Appearance | Settings → Navigation; vault-specific rows explicitly name the vault |
| Note width / zoom / wrap / Focus / Typewriter | View, document menu, toolbar, More; some have no Settings row | View commands; persistent defaults searchable under Reading or Editing |
| Vault opening behavior | Appearance, open-vault dialog | Settings → Vaults → Opening behavior |
| Vault list / rename / color / default / remove / daily notes | Settings cards, vault-name menu, current card More | Settings → Vaults; compact searchable list plus selected-vault details |
| Keyboard Shortcuts | Read-only shortcut sheet and editable Settings section | Settings → Keyboard Shortcuts; help sheet remains a contextual reference |
| Sync / share / pairing | Features, More, startup, Settings and status banners | Settings → Connections → Sync; existing entry points deep-link here |
| Agents / T3 / Second brain | Features, More, Project onboarding, Settings | Settings → Connections; module availability and explicit enablement stay visible |
| Logs / Kibana / HTTPS sources / editor paths | More → Logs, Settings → Logs, log view tools | Log commands stay local; preferences and sources in Settings → Logs |
| Inbox | More app navigation and Settings placeholder | Navigation command; keep the unconnected state explicit |
| pomoi upload / token / image import / paste limits | Settings → Editor | Settings → Editing → Files and images |
| Service storage / diagnostics / recovery | Help, More, Manager, Settings details | Advanced within the relevant category; keep recoverability reachable |

Duplicates are often useful entry points. The problem is separate lists and
inconsistent names, not the existence of contextual shortcuts. A shared command
identity should supply label, category, availability, shortcut and target scope
to every presentation. Search returns one result per identity with a breadcrumb.
"Copy path" currently means different formats in different surfaces; it must
not be collapsed into one identity until the payload is explicitly distinguished.
Zoom changes a multiplier; Text size changes a base font size. They are related,
not duplicate settings. Vaults in Sync describes sharing, not the known-vault list.

## Three alternatives

| Choice | Menus | Settings | Benefit | Cost / risk |
| --- | --- | --- | --- | --- |
| A · Grouped sidebar (recommended proposal) | Search above File, Edit, View, Navigate, Connections, Help; secondary submenus | Familiar left sidebar with named groups and global search | Short learning path; preserves existing Settings window and native macOS menus | Still needs a clear shared registry to prevent future duplicate lists |
| B · Task hub | Search plus task categories: Create and open, Work on a note, Organize vaults, Connect computers, Troubleshoot | Task index opens focused Settings pages with breadcrumbs and search | Starts from what a user wants to do; Vaults has direct access | Some settings fit several tasks; search aliases and a canonical home become essential |
| C · Command first | Searchable command list with category filters; browse is always available | The same finder has a Settings filter and opens the owning Settings page | Fast for frequent users; one discoverable search entry point | Commands and preferences can be confused; result types and context labels must be explicit |

All three retain native macOS menu conventions and provide the same searchable
command access on Windows and Linux. Context menus retain the clicked object,
group Copy / Open / Organize, and place destructive actions last. Search never
silently enables a feature or changes a preference. A result opens the owning
section and highlights the row; execution remains explicit.

## Search and Vaults behavior to preserve in implementation

Search indexes labels, descriptions, category paths and intentional aliases
(e.g. "theme", "dark", "vault", "folder", "sync", "shortcut"). Unavailable
platform/module results explain why and offer the relevant setup page, without
performing setup. A query with no matches has a clear empty state and Clear.
Global Settings search is independent of a vault-list filter and the shortcut
editor's local search. Clearing search returns to the previous section.

The Vaults page keeps the current and default badges, full path, last opened
time and checking/unavailable state. Search matches name and path. The list
offers recent/name ordering. Row selection shows details inside Settings;
Open, Open in new window, Make/Clear default and Remove from list remain
explicit. Removal keeps files and reading state. Rename, color, templates,
hidden files and reminders retain their existing scope; selecting an inactive
vault must not silently apply changes intended for the current vault.

Keyboard acceptance: predictable Tab order, visible focus, arrow navigation,
Enter to activate, Escape to leave search/menu and return focus to the opener.
Native shortcuts and user keymap overrides remain authoritative. Long labels,
large fonts and the minimum Settings window size need native acceptance on each OS.

## Decision record and proposed implementation issues

**No structure has been chosen by Oleg yet.** A is a recommendation, not an
approval. Record the selected alternative, any amendments and the issue-comment
link here and in #3107 after Oleg chooses. Do not infer a choice from this PR.

Proposed split, to create after that choice (no issue numbers allocated yet):

1. Shared command inventory and searchable grouped application menus; preserve
   native macOS menu bar, shortcuts, availability and all existing actions.
2. Settings categories, global search and row deep-links using the chosen layout;
   preserve preference storage and module enablement semantics.
3. Vaults list inside Settings: filtering, ordering and selected-vault details;
   preserve current/default/unavailable states and non-destructive removal.
4. Context/document/viewer menu grouping and naming using shared identities;
   retain clicked-target scope, read-only boundaries and platform-specific actions.

Each implementation issue needs native Mac/Windows/Linux evidence, search empty
and unavailable states, keyboard navigation and existing-action coverage. This
docs PR fulfills the writer's inventory/mockup delivery; Oleg's choice and actual
product implementation remain pending and are not claimed as delivered behavior.
