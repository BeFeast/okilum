# Domain views

Layout groups the sidebar; it never hides a file. Without a vault-authored
sidebar, Areas lists the declared domains, each with the number of files in
its folder and a disclosure that expands into that folder as it is on disk
(folders first, `@`/`.` items hidden as in the Tree). **All files** follows
the domains and lists the whole vault as the Tree does, so anything outside a
domain (inbox, sources, knowledge, log, undeclared folders) stays reachable.
A domain row's name still opens its domain view; files open as from the Tree.

Areas opens the built-in `@views/domain.md` for a domain when the sidebar
definition omits `node_view` or points at `[[@views/domain]]`. A vault file at
`@views/domain.md` supplies the template instead. A custom `node_view` can still
point at any supported template under `@views/`.

The built-in accepts `domain` (an area) and `context` (optional). A context
scopes the state, log and open task sections. The default value `-` selects
all contexts, including domain-wide records. Inbox items match the lowercase
domain tag exactly. Sources match the projected area. Both sections stay
domain-wide because their lifecycle layers have no context.

## Gardener snapshots

For Health, save an instance as `@views/domain-health.md`:

```yaml
---
type: view
extends: "[[@views/domain]]"
args: {domain: Health}
materialize: true
---
```

Gardener uses the instance filename to publish
`@views/out/domain-health.json`. The instance uses the same template and vault
override as Areas. To inspect the JSON that Gardener consumes, run
`okilum-cored view @views/domain-health.md --format json` with the vault
selected. The command reads the vault and emits JSON to standard output.
