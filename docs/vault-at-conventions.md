# Additive special-path conventions (#2821)

`@` is a vault data prefix alongside `_`. Visibility never makes a path a
service file, excludes it from Sync, changes canonical bytes or migrates a vault.
`.okilum` metadata and durable identities keep their existing locations.

The shared conventions classify hidden path segments and case-insensitive
`@index.md` / `_index.md` names. Exactly one spelling from the `@` family wins
over every legacy spelling. Multiple spellings in the preferred family remain
ambiguous. Index rows and the Vault Index header use that decision; legacy-only
folders retain their existing behavior. Quick open/backlinks use folder titles.
Bare `[[@index]]` prefers an unambiguous @ index in the linking note's folder;
without a local @ index it uses the existing resolver fallback. Multiple local
@ spellings remain ambiguous. Explicit relative, qualified and root paths keep
their existing meaning. The original issue described legacy bare `[[_index]]`
as folder-relative, but the actual resolver is root-first. Legacy `_index` and
unrelated bare-name resolution deliberately remain unchanged.
Projects and workbench consume the same families. Project creation inspects both
families before writing and defaults to `@index.md` only when the vault signals
that convention with a root `@index` or `@Assets` / `@Schema` directory.

Markdown anywhere below root `@Assets/` is asset content: excluded from note
lists/counts, note search, Quick open and workbench activity. Markdown identities
and revision-bound sources remain available to opening, embeds and move/rewrite.
Both full scans and cached/incremental publications preserve this separation,
including deletion of a containing directory. Ordinary `@` notes remain notes.
The legacy `_Assets/Daily Notes/` exception is unchanged.

The templates branch retains its existing visibility exception. Settings and
Obsidian template settings win; otherwise the default is existing
`@Schema/Templates`, then existing `_Assets/Templates`, then `@Schema/Templates`.

All paste destinations use a typed Image/File resolver. An explicit usable
Obsidian `attachmentFolderPath` wins. Next: existing `@Assets` gives
`Images`/`Files`; existing `_Assets/Attachments`; existing root `attachments`;
finally new `@Assets/Images` or `@Assets/Files`. Resolution creates nothing.
Existing no-replace writers create destinations on demand. Mixed file pastes
retain a destination per item; extensionless images use the existing magic-byte
recognizer. Existing `@Assets` files link in place. The dot-only copy rule stays.
The capture attachment publisher already receives its explicit immutable folder
from its caller; the desktop capture composer currently has no attachment caller.

Root `@AgentContract.md` participates in Rules and agent diagnostics while
remaining an ordinary default Quick open result. Both `@@AgentContract.md` and
`@./@AgentContract.md` imports resolve. Language policy prefers
`@Schema/language.yaml`, with `_Schema` fallback. Both guarded native shared-ignore
readers prefer `@Schema/stignore.template`, falling back to `_Schema` only when
absent. An empty current template wins; read errors remain errors.

## Reproduction and evidence

Generate an entirely synthetic writable fixture in an empty destination:

```sh
python3 scripts/qa/at-vault-fixture.py /path/to/empty/synthetic-vault
```

The same JSON manifest drives Rust fixtures. A real filesystem case probe adds
`Twice/@index.md` + `Twice/@Index.md` only on case-sensitive filesystems; pure
inventory tests exercise both case families on every host. The helper refuses a
nonempty destination. No private vault data, identities or paths are included.

Expected native evidence:

- Show hidden off: no `@Assets`, `@Private`, `_Private` or root contract rows;
  the configured `@Schema/Templates` ancestor branch is the documented exception.
  Area, Legacy and Mixed have their index row first; root `@Index.md` is Vault Index.
- Show hidden on: hidden paths have the existing muted marker. Clicking Mixed
  opens its `@index.md`; Twice remains Several where supported.
- An image paste writes `@Assets/Images`, a PDF paste writes `@Assets/Files`.
  A mixed paste keeps both destinations. Template Note comes from `@Schema`.
- Quick open/search find Area and ordinary `@Private` notes; Markdown assets
  remain absent. Links open the index and drawing/Markdown asset embeds resolve.

Hosted tests cover Linux and native Mac/Windows readers and fixture filesystem
behavior. They do not prove installed-beta GUI acceptance. The owner performs
real-vault read-only acceptance separately. Standalone non-Markdown attachment
rename remains outside the existing Markdown MovePlan; directory moves rewrite
attachment references through the existing path.
