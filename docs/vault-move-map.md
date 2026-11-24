# Reader state across a vault cutover

Gardener publishes the sealed lookup at `@views/path-map.jsonl` in the successor vault. Each record includes `old_path` relative to the archive and `new_path` relative to the successor, with `null` for retirement. The archive's `.okilum/vault.json` declares the successor's vault UUID. The archive marker is `.brain-read-only`.

Okilum verifies the two distinct vault identities and consumes exact sealed map rows. On session restore, a resolved Brain selection opens in a Grove window. Opening Grove imports resolved Recent entries, pins, saved navigation and project preferences into its own app state. A retired or ambiguous shortcut remains associated with Brain and appears with a **moved?** marker in Grove. Existing Grove shortcuts keep their order. Once imported, removing a pin in Grove stays effective through restart.

For ordinary renames within a vault, Okilum uses saved filesystem identity and creation time, then a unique SHA-256 match. This evidence is captured when the original is accessible. Bulk changes are reconciled in the background. Gardener owns canonical link rewrites for the root switch. Okilum reads the resulting Markdown and refreshes embeds against the published inventory.
