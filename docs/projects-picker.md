# Shared Projects picker (#1520)

`okilum_shell::projects_picker` is the mountable GPUI component. It depends on
`okilum_t3::projects::picker::PickerModel`, which consumes the existing #1481
`Discovery`/`Project`/`Kind` types; there is no second manifest parser or scanner.
The component does not depend on Reader, main.rs, or manager/right-panel code.

## Projects section (wired)

The Projects app section (`reader_project_navigation.rs`) renders this picker
with `Presentation::Page`: own search and kind filter, left-aligned (max 760 px),
filling the section. `Selected` goes through `enter_vault_project`; a successful
entry records the folder in the per-vault sidebar state (`recent_projects`,
vault-relative) and calls `record_opened`, so recents survive relaunch. Live
statuses from `projects::statuses` are fed through `update_statuses`; the dot
tooltip carries the live reason (for example "not connected to T3").

Manifest problems never print raw red text: a folder whose `ops/project.toml` (legacy `ops/project.yaml` while TOML is absent)
cannot be read is a muted row with a small warning sign inside its domain, the
reason in a tooltip (hidden under a kind filter, since its kind is unknown). A
manifest whose `kind` is not a project kind (`operating-area` in Dev intake) is
skipped quietly by discovery. Manager ids are any path-safe T3 thread id: both
`mcp:<id>` and the plain UUIDs created in the T3 UI.

For the #1519 title-bar switcher:

1. Reuse `projects::discover(vault)` on the existing background executor. Construct
   `PickerModel::new(vault, discovery, recent_folders)`; history is most-recent-first
   and scoped to that vault. Create `ProjectsPicker::new(model, Presentation::Popover,
   window, cx)` in an Entity.
2. Render `mount(entity, switcher_left_bottom_in_window_coordinates)`. It anchors
   left, fits the viewport, focuses search, and supplies kind filters, collapsible
   domains, arrows/Enter, clearable search and Escape/outside-dismiss events.
3. Subscribe to `Selected(Project)` and call the existing revalidating project
   activation pipeline. Only after successful entry call `record_opened(folder,cx)`;
   persist `RecentsChanged(Vec<PathBuf>)` in the host's UI state, per vault. Failed
   or cancelled entry must not become recent. Set the current check with `set_active`.
4. Subscribe to `DismissEvent`, remove the transient component and restore the
   host's native focus. Recreate it for a different vault; do not reuse old discovery
   or status work across vault generations.

For #1518's Projects palette section, use the same component with
`Presentation::Embedded`. It renders the same grouped rows without stealing the
palette's search/focus or adding a second search field. Feed the palette query
through `set_query`; kind filtering uses `set_filter`. Route arrows to
`move_selection` and Enter to `confirm` when that section owns the palette selection.

The host delivers live manager readings using `update_statuses`, with a
`ManagerObservation { manager, status }` tied to the pointer used for the listing.
A rotated manager invalidates the old green dot. Not-started, idle, unreachable and
checking are distinct; a manifest pointer alone never means running. No new network
or launch operation happens when rendering or choosing a row.

Five confirmed recents appear first, also remaining in their alphabetic domain
groups, as in the approved #1508 mockup. Domain is the first vault-relative
component of the discovered manifest's project folder: Music/Areas and
Music/Projects both belong to Music. A vault-root manifest uses Vault. Search
matches name, vault-relative location and kind across all groups; empty groups
vanish. Search reveals matches inside collapsed groups and clearing it restores
collapse choices. Problem rows stay in their domain, apart from healthy rows.

Unit tests exercise real discovery/manifest defaults, domains, recency/order,
search/filter, collapse and stale-manager protection. GPUI component tests cover
left anchoring, real search/arrow/Enter/Escape events, activation history and a
palette refresh that removes the selection. These are regression evidence;
ordinary native/visual acceptance remains human QA on a concrete beta build.
