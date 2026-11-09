# Context service-file storage (#2772)

The owning Project/Area, its service directory and the existing T3 workspace
are separate identities. Relocation changes only the directory holding the
application metadata. It does not create a Manager, change a workspace, convert
YAML/TOML, regenerate local-card UUIDs or relocate knowledge notes.

## Resolution and creation

`okilum_t3::service_layout::ServiceLayout` resolves the complete set:
`project.toml`/`project.yaml`, `board.toml`/`board.yaml`, `board-local.toml`,
optional `board-context.toml`/`board-delivery.toml`, and the existing
`thread-ids.md` registry where used. Its current registry/manifest precedence is
unchanged. Local cards are durable user data, even though their file is named
`board-local.toml`; they are not disposable cache.

New explicit project creation uses `<context>/.okilum/project.toml`. A legacy-only
project continues to read and write its existing `<context>/ops` directory.
Readers never create either directory. Existing `.okilum/vault.json`, icons and
other settings do not by themselves select the project layout, and are never
replaced by relocation.

One directory owns the whole service-file set. Both locations containing service
files is a visible conflict, including identical duplicates or an orphan sidecar
beside the other location's project. Missing optional files stay missing; no
consumer borrows legacy cards from a different directory. Orphan sidecars without
a project, conflict copies and interrupted migration are errors, not invitations
to create a second project. Within the selected directory TOML is preferred;
only absence permits YAML fallback. Invalid, unreadable or symlink metadata does
not select another file or location.

The resolver supplies creation, manifest and first sidecar writes, registry,
Board context/delivery, source links and generated Manager guidance. Edits retain
the selected location token and exact source revision; a location switch or
competing revision requires Reload. Ordinary `ops` runbooks/decisions/daily notes
remain knowledge and never become part of a location migration.

External tools can use the same read-only resolver without GUI startup:

```sh
okilum service-path /path/to/owning-context project
okilum service-path /path/to/owning-context board-local
okilum service-path /path/to/owning-context thread-ids.md
```

It returns JSON with the context, selected directory/layout and actual file path.
Absent files and conflicts fail explicitly; scripts must not fall back to a
hardcoded `ops` path. A display/highlighting language or vault root is not a
substitute for the owning context.

## Metadata notifications

Service paths stay outside note inventory/search. `Changes.metadata` separately
reports owning contexts on external metadata writes, create/delete, conflict
copies and directory-location changes. Note/index input is empty for metadata-only
batches. Duplicate windows share the metadata publication counter. Project
discovery and Board/Manager models invalidate outstanding generations before
refresh; stale results cannot publish fresh availability. Ordinary `ops/*.md`
knowledge continues through the normal note watcher.

## Explicit migration and rollback

Open **Project service storage…** from the Reader menu or Help. Select the actual
owning folder relative to the open vault; the vault root is not assumed to own a
nested Project/Area. **Review storage** builds a no-write plan of the complete
file set, original revisions, sizes, SHA256 and source/destination directories.
Existing YAML remains YAML, byte-for-byte, including comments and unrelated data.

Before **Apply reviewed plan**, explicitly confirm that every used app channel,
external reader/agent and synced device supports this layout, and competing
metadata editors/Sync writers are paused. A bound Manager's workspace and
credentials are not part of this operation. Both a corporate-style nested Area
and an existing application-development context use this same procedure. Their
real paths, identities and plans belong in private records, not public fixtures.
Neither real context is migrated merely by installing the supporting beta.

Apply holds the cooperative cross-process context lock and checks the reviewed
source/location revisions again. Exact backups and a transaction journal live in
local durable app data under `service-layouts`, outside the vault and deletable
index. Only whitelisted service files enter transaction staging; the complete
`ops` or `.okilum` folder is never moved or replaced. Original permissions and
file modification times are retained. Existing icon/vault settings, notes and
note dates stay intact.

A vault marker blocks normal service readers/writers while files are staged and
published. The new project layout is not made actionable halfway through.
Recovery is explicit through **Review storage**, **Resume migration**, or
**Review rollback** then **Apply rollback**. Unknown/mismatched revisions remain
conflicts, with backups and unexpected bytes retained for reconciliation. Local
writer admission is cooperative; it does not secretly stop external programs or
claim to quiesce other devices. Old clients must be updated before Apply.

After success the receipt and backups remain local. **Review rollback** prepares
the reverse plan only if the complete committed output is unchanged, including
its revisions. A newer edit refuses automatic rollback rather than replacing it
with an old backup. The reverse Apply is journalled too. A location round trip
invalidates pre-migration editor tokens even if the original bytes are restored.

Relocation and the [YAML/TOML format transition](area-manifests.md) are independent
operations. Supporting both locations in source or parsing a configuration is
not delivery proof: containing official builds and actual native fixture
discovery/Board/Manager/edit/reopen evidence are required. Real migrations each
need their own concrete reviewed plan and separate explicit user Apply.
