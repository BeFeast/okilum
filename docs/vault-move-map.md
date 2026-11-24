# Reader state across a vault cutover

Gardener publishes `@views/path-map.jsonl` in Grove as headerless JSON Lines. Each object contains `old_path`, `new_path`, `action`, `rule`, `reason`, and `aliases`. Paths are relative to their respective vault roots. A `null` `new_path` means the source was retired. `aliases` supplies extra keys for user-entered lookups such as Gardener's `brain where` command.

Okilum checks Brain's link identity (`cc480ea3-0b98-4783-960f-3a64fa4c274c`) and its `.brain-read-only` marker. The archive's `.okilum/vault.json` must identify a read-only archive and declare a successor UUID matching Grove's `.okilum/link-identity.json`. These are the cutover identity checks, with no seal file required.

Restoring app state matches saved paths against `old_path` only. Gardener emits a single directory row for each opaque tree, using `keep-nested` for embedded Obsidian vaults and `keep-unit` for whole units such as `repo-seed` and `anton-mbp-homelab`. Okilum replaces the matching directory prefix at a path-component boundary and keeps the descendant path. Gardener preserves the contents of those trees byte for byte.

On session restore, a resolved Brain selection opens in Grove. Opening Grove imports resolved Recent entries, pins, saved navigation and project preferences into its own app state. Retired, ambiguous or unresolved shortcuts remain associated with Brain and appear with a **moved?** marker in Grove. Existing Grove shortcuts keep their order. Once imported, removing a pin in Grove stays effective through restart.

For ordinary renames within a vault, Okilum uses saved filesystem identity and creation time, then a unique SHA-256 match. This evidence is captured when the original is accessible. Bulk changes are reconciled in the background. Gardener owns canonical link rewrites for the root switch. Okilum reads the resulting Markdown and refreshes embeds against the published inventory.
