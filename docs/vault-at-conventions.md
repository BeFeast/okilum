# Special-path conventions (#2847, superseding #2821)

`@` is the vault data prefix. `_` has no path role: underscore files and folders
are ordinary visible content, including `_Private`, `_export`, `_index.md`,
`_Assets` and `_AgentContract.md`. Dot paths remain hidden application-state
paths where applicable. Visibility never changes canonical bytes, excludes data
from Sync or migrates a vault. `.okilum` metadata and durable identities retain
their existing locations.

Only case-insensitive `@index.md` is a folder index; multiple case spellings
remain ambiguous. Root `@Index.md` supplies the Vault Index header. `_index.md`
is an ordinary note with its own title, no preferred row or folder activation.
Quick open/backlinks use folder titles for @ indexes. Bare `[[@index]]` prefers
an unambiguous @ index in the linking note's folder, then the existing resolver
fallback. `[[_index]]` has ordinary root-first name resolution. Explicit relative,
qualified and root paths retain their existing meaning.
Project discovery reads `type: project` from @ indexes only. Project creation
always writes `@index.md` when no @ spelling exists, preserving any ordinary
`_index.md` bytes. Workbench has no `_schema` / `_Assets` noise rule or
`_Assets/Daily Notes` exception; these paths are all ordinary input.

Markdown anywhere below root `@Assets/` is asset content: excluded from note
lists/counts, note search, Quick open and workbench activity. Markdown identities
and revision-bound sources remain available to opening, embeds and move/rewrite.
Both full scans and cached/incremental publications preserve this separation,
including deletion of a containing directory. Ordinary `@` notes remain notes;
all `_Assets` Markdown remains ordinary note content.

Templates and their ancestors follow the same hidden-files preference as other
paths (#2844). Show hidden files reveals special paths muted with the marker;
ordinary configured template folders stay visible. New note from template and
Settings retain access regardless of tree visibility. Settings and Obsidian
`.obsidian/templates.json` win; otherwise the default is `@Schema/Templates`.
There is no `_Assets/Templates` fallback; explicit configuration may still select
any ordinary underscore folder.

All paste destinations use a typed Image/File resolver. An explicit usable
Obsidian `attachmentFolderPath` wins. Next: existing `@Assets` gives
`Images`/`Files`; existing root `attachments`; finally new `@Assets/Images` or
`@Assets/Files`. There is no `_Assets/Attachments` fallback. Resolution creates
nothing. Existing no-replace writers create destinations on demand. Mixed file
pastes retain a destination per item; extensionless images use the existing
magic-byte recognizer. Existing `@Assets` files link in place. The dot-only copy
rule stays. The capture attachment publisher receives its explicit immutable
folder from its caller; the desktop capture composer has no attachment caller.

Only root `@AgentContract.md` participates implicitly in Rules and agent
diagnostics. Search treats it as an agent file: the Agent files chip lists it
and the default Quick open and content search views leave it out, Pinned and
Recent included (#2940). `Area/@AgentContract.md` and `_AgentContract.md` stay
ordinary results. Both
`@@AgentContract.md` and `@./@AgentContract.md` imports resolve. An explicit import
may still name any ordinary file. Language policy uses `@Schema/language.yaml`;
both guarded native shared-ignore readers use `@Schema/stignore.template`.
Neither reader falls back to `_Schema`. When the @ template is absent but
`_Schema/stignore.template` exists, creating a shared policy from defaults is
refused with a message to move that file to `@Schema/stignore.template`. This
prevents silently dropping fleet exclusions. Existing saved shared policies and
local `.stignore` bytes stay untouched. An empty current template wins; read
errors remain errors.

## Release note and tester warning

Release notes and What's new are generated from the merged PR body's authored
`User-visible:` line by `scripts/releases/release_notes.py`; `CHANGELOG.md` is
published Stable history. The #2847 PR includes this warning in that canonical
line, and the tester announcement for its first beta must carry it too:

> Underscore paths are now ordinary visible files and folders. Vaults still using
> `_index.md` / `_Assets` lose folder indexes, default templates and project
> discovery until their owner renames them to the @ conventions (`@index.md`,
> `@Schema/Templates`, `@Assets`). Shared Sync policy creation is blocked when
> only `_Schema/stignore.template` exists: move it to `@Schema/` first. Existing
> saved policies and local `.stignore` are unchanged. Okilum does not rename or
> migrate vault files.

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

- Show hidden off: no `@Assets`, `@Private`, `@Schema` or root @ contract rows.
  `_Private`, `_Assets`, `_Schema`, `_AgentContract.md` and `_index.md` stay visible
  without hidden styling. Area and Mixed have their @ index row first; Legacy
  has an ordinary `_index.md`. Root `@Index.md` is Vault Index.
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
