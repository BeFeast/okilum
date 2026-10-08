# Research: knowledge organisation model for navigation (#370)

Status: option B approved by Oleg on 2026-10-07. The accepted Projects slice is
recorded in `docs/design/reader.md`; later Views/service/connector phases below
remain separate follow-ups.

Question: how should Tessera's sidebar organise a vault, now and once the
backend service and connectors (Maestro, Linear, Notion-like systems) supply
tasks, projects, status and ownership, without converting or rewriting
canonical Markdown?

## Fixed inputs

These are owner decisions or approved contracts. The options below do not
reopen them.

- **The vault is the source of truth.** Plain Markdown, editable without Tessera
  (PRD §2). Indexes and projections are derived and rebuildable.
- **The Tessera backend service is the engine.** It indexes the vault, owns
  reminders, agent questions and the phone surface. Structured views live in the
  service (and in app state), **not** as files in the vault — no `.base`-style
  view files, no generated dashboards.
- **Projects are folders with `_index.md`** carrying `type: project` in
  frontmatter. **Tasks are Markdown checkboxes** (with the Tasks emoji metadata
  already parsed by [Tasks query blocks](../tasks-query-blocks.md)).
- **Identity is the path** (PRD §4). Ambiguity is surfaced, never resolved
  silently.
- **Durable operational state** (external identities, bindings, cursors) is not
  index data and must not live under the deletable index directory (AGENTS.md).
- **Current sidebar** ([reader.md](../design/reader.md), Sections):
  Recent / Pinned / Inbox (computed) / Folders (real directories only, `_` and
  `.` folders hidden, archive folders muted).
- **The owner's vault:** PARA (`Projects` / `Areas` / `Resources` / `Archive`)
  inside each domain, a per-domain `_index.md` and `AGENTS.md`. `_index` alone
  occurs 272 times in the 3933-note measurement (PRD §4), so the folder-plus-index
  convention is already the dominant structure, not an aspiration.

## TL;DR

- Of the Second Brain methods, **PARA is the navigation model**. **CODE**
  (and GTD's capture → clarify) is a *flow*: it belongs in Inbox and review
  views, not in the tree. **MOCs** are what `_index.md` already is.
  **Zettelkasten** belongs in links, backlinks and search, not in the
  sidebar.
- The apps that navigate well (Linear, Things, Finder, Obsidian with few
  plugins) share one pattern: **a short fixed list of *places* above one
  canonical hierarchy**, with saved views as the extensibility point. The apps
  that get noisy (Notion, Tana, Capacities, Logseq) mix types, queries,
  favourites and hierarchy into one tree, or make the user design their own
  navigation.
- **Recommendation: option B, "Projects as a place".** Keep Folders as the
  truth. Make project folders recognisable in the tree. Add one derived
  **Projects** section (all `type: project` folders across domains, grouped by
  status). Later, connectors *annotate* those rows and add saved **Views**,
  without writing to Markdown. It is a small step from what shipped in #369.
  Each later step can be tried, and backed out, on its own.

## 1. Second Brain practices: what fits this model

| Practice | Core idea | Fit for Tessera navigation | Where it belongs |
| --- | --- | --- | --- |
| **PARA** (Forte) | Organise by *actionability*: Projects (with an end) → Areas (ongoing responsibility) → Resources (interest) → Archive | **High.** The vault already uses it. Folders map 1:1; `type: project` makes the P machine-readable | Folder tree + a derived Projects section |
| **CODE** (Capture, Organise, Distil, Express) | A *pipeline* from capture to output | **Medium, as a flow.** Capture = Inbox (already computed). Organise = moving into PARA. Distil/Express are writing activities, not places | Inbox reasons, a later Review view; no sidebar section per stage |
| **GTD inbox / weekly review** | Empty the inbox; review every project for a next action | **High for views.** "Project with no open task" and "project untouched for 30 days" are exactly the derived signals a service can compute | Inbox (exists), Review (later, service) |
| **MOCs / LYT home notes** (Milo) | Hand-written hub notes that link a topic's notes | **High, already present.** `_index.md` *is* the MOC of its folder | Folder row opens its `_index.md`; Contents and Linked from do the rest |
| **Zettelkasten** (Luhmann) | Atomic, linked, ID'd notes; structure emerges from links | **Low for the sidebar, high for the pillars.** Its value is links, backlinks, search, transclusion — PRD pillars 2–3 | Right panel, search, link preview. No "Zettels" section, no ID scheme imposed |
| **Johnny.Decimal / numbered taxonomies** | Fixed numeric hierarchy | Not used in this vault | Nothing to do; folder names sort naturally |
| **Daily / journal-first** (Logseq, Roam) | The day is the entry point | **Low as a default.** The vault has dated notes, but work is organised by domain and project | A Today place only once the service owns reminders |

Conclusion: navigation should express **PARA plus two computed lenses**: what
needs processing (Inbox, later Review) and what is in flight (Projects,
later Today). Everything else is already served by links, search and the
right panel.

## 2. How comparable apps navigate

Rated against Tessera's constraints: file-backed, folder truth, minimal UI.

| App | Sidebar model | What works | What is noise for Tessera |
| --- | --- | --- | --- |
| **Obsidian** | File explorer + Bookmarks + Search; Bases (2025) add database views over properties | One honest tree; bookmarks; properties as plain YAML | Views stored as `.base` files in the vault (contradicts the "views live in the service" decision). Folder notes and dashboards depend on plugins, so every vault differs |
| **Logseq** | Journals home, All pages, Favourites, Recent | Low-friction capture into the day | Block outliner model; pages without hierarchy; DB version moves truth out of files |
| **Tana** | Home, Today, Supertags, Pinned, workspace | Supertags make "all projects" a live query; good inbox | Everything is a node; the user designs the schema; heavy cognitive load; not file-backed |
| **Capacities** | Object types (Pages, People, Books…) as top-level entries, Daily notes | Browse *by kind* is intuitive for collections | Type-first replaces hierarchy; no folders; one section per type grows without bound |
| **Notion** | Search, Home, Inbox, Favourites, Teamspaces, private page tree | Databases with saved views; page = folder | Unlimited nesting; favourites and trees duplicate one another; sidebar bloat; database rows are not files |
| **Bear** | Notes, Untagged, Todo, Today, nested tags | Computed smart lists (Todo, Untagged) are tiny and useful | Tags-as-folders: hierarchy lives inside note text |
| **Craft** | Spaces, folders, Daily notes, Tasks (Inbox / Today / Upcoming / Logbook) | Clean task places beside documents | A second task system parallel to documents; card-heavy UI |
| **Heptabase** | Whiteboards, Card library, Journal, Tags | Spatial sense-making for research | Whiteboard-first; tag databases are app-owned |
| **Linear** (non-note reference) | Inbox, My issues, Views, Teams › Projects / Issues | A fixed set of places; **saved views** as the only extension; status and owner as compact glyphs | — (the model to follow for derived work views) |
| **Finder / Raycast** (reference) | Favourites + locations; palette-first | Navigation is boring and fast; the palette carries breadth | — |

Patterns to borrow:

1. **A few fixed places, then the hierarchy** (Linear, Finder, Things). Places
   are computed; the hierarchy is the user's.
2. **Smart lists over configuration** (Bear Todo/Untagged, Tessera's Inbox).
   A computed list with a reason per row beats a setting.
3. **Saved views as the only extension point** (Linear Views, Notion database
   views), stored as app/service state.
4. **Status and ownership as glyphs, not columns** (Linear): one status
   glyph, an avatar or initials, and a due pill. No grid.
5. **The palette carries breadth** (Raycast, ⌘K). The sidebar does not try to
   list everything.

Patterns to avoid: type-first navigation that hides folders, user-designed
schemas as a prerequisite, unlimited nesting of favourites, view definitions
written into the vault, and per-type sections that grow without bound.

## 3. Derived views from connected systems

When Maestro, Linear or a Notion-like system is connected, it carries tasks,
projects, status and ownership that partly overlap Markdown. Rules that keep
Markdown canonical:

- **Annotate, never convert.** External items appear as *annotations* on
  Markdown projects and tasks (a status glyph, owner, linked issue count), or as
  rows in a service-owned View. Tessera does not generate notes, rewrite
  checkboxes or mirror external items into files.
- **Explicit binding, stored as operational state.** A project folder is bound
  to an external project by an explicit user action, as Maestro linking already
  is ([ai-brain-maestro-native.md](../ai-brain-maestro-native.md): "Discovery
  never links implicitly"). The binding (folder path ↔ external id, the
  provider-reported identity, the observation cursor) lives in the service's
  operational directory. It is not frontmatter and not under the index
  directory.
- **Read existing keys, never require them.** If a note already carries
  `linear: ENG-12` or a URL in frontmatter, the service may *propose* a
  binding from it. Writing such a key back is an optional, explicit,
  revision-aware source edit ([ai-brain-contracts.md](../ai-brain-contracts.md)),
  never a side effect of connecting.
- **Each fact keeps its authority.** Note content and `status:` in
  `_index.md` are Markdown's. Issue state, assignee and cycle are the
  provider's. (Todoist stays task authority during the transition, PRD §11.)
  When they disagree (the `_index.md` says `active`, Linear says `Completed`),
  the row shows both with a quiet divergence marker. Tessera never picks one,
  following the ambiguity rule.
- **Stale is visible.** An annotation shows its last observation. A
  disconnected provider keeps the last evidence, marked as old, rather than
  blanking the row, matching the Maestro observation behaviour.
- **Rebuildable split.** Markdown-derived facts (project list, open
  checkboxes, due dates) are rebuildable from the vault. Provider facts are
  rebuildable from the provider plus the binding. Only the binding and cursors
  are durable state.

## 4. Options

All three keep Recent, Pinned, the computed Inbox and the real folder tree, and
follow the UI rules: 28 px rows, glyphs instead of labels, no zero counts, no
`.md` or raw paths, no bordered tables. Counts appear only when nonzero.
Sketches show the sidebar at its 264 px default width.

### Option A — "Project-aware folders" (minimal)

No new section. The tree learns one fact: a folder whose `_index.md` says
`type: project` is a **project**.

```
 Vault                    ⌕ ⌘K
 ─────────────────────────────
 ▸ Recent
 ▸ Pinned
 ▾ Inbox                     3
     Call notes      at root 2d
     Pricing idea    no links 5d
 ▾ Folders
   Work
     ▾ Projects
         ◐ Tessera            12
         ◐ Halenote            4
         ○ Office move
     ▸ Areas
     ▸ Resources
   ▸ Home
   Archive
```

- `◐` / `○` / `●` are status glyphs from `status:` (active / planned / done).
  The trailing number is open checkboxes in the folder. It is hidden at zero.
- Clicking a project row opens its `_index.md` (the folder note). Disclosure
  still expands it. `_index.md` and `AGENTS.md` are not listed as children; they
  remain reachable via the row, search and More → "Agent instructions".

Pros: smallest change; no new concept; keeps one place for everything; works
fully offline from the local reader index.
Cons: projects stay scattered across domains — "everything I'm working on" still
needs the palette or a hand-written MOC. No home for connector views.

### Option B — "Projects as a place" (recommended)

Option A, plus one derived **Projects** section between Inbox and Folders, and
a reserved slot for saved **Views** that appears only once one exists.

```
 Vault                    ⌕ ⌘K
 ─────────────────────────────
 ▸ Recent
 ▸ Pinned
 ▸ Inbox                     3
 ▾ Projects
     ◐ Tessera        Work   12
     ◐ Halenote       Work    4  ⚠
     ◐ Kitchen        Home    1
     ○ Office move    Work
     Show 6 done
 ▸ Views                          ← only when a view exists
 ▾ Folders
   Work
     ▸ Projects
     ▸ Areas
   ▸ Home
   Archive
```

- **Projects** = every `type: project` folder outside the archive, across all
  domains, grouped active → planned (with an expandable "done" group), sorted by
  recent activity. The muted domain label disambiguates equal names
  (the same rule as "Linked from"). The count is open tasks, hidden at zero.
- `⚠` appears only when there is something to say: an ambiguous or invalid
  `_index.md`, or (later) Markdown status diverging from a bound provider.
  Hover explains it.
- Selecting a project opens its `_index.md`. The tree reveals the folder, as
  for pinned folders today.
- **Views** (phase 2+): service-owned saved filters such as "Open tasks due this
  week", "Projects without a next action" (GTD review) or "Assigned to me in
  Linear". The service stores them, they render with the existing task-result
  rows, and they never become files.
- With a connector bound, a project row may gain an owner avatar and a
  provider status glyph *next to* the Markdown glyph, not replacing it.

Pros: answers "what am I working on" in one glance, which is the main gap in
the current sidebar; follows PARA's own priority (Projects first); one obvious
extension point for derived views; the Projects section degrades to option A
behaviour when the service is absent.
Cons: one more section to fold (#434 folding already handles this); a project
appears twice (in Projects and in its folder), which is the same accepted
duplication as Pinned; depends on `type: project` being consistent. A
"Projects without type" diagnostic may be needed during adoption.

### Option C — "Work / Library" split

The sidebar gets a two-segment switch at the top. **Work** holds computed
places; **Library** holds the PARA tree. Closest to Linear or Craft.

```
 Vault                    ⌕ ⌘K
 ─────────────────────────────
 [ Work │ Library ]
 ─────────────────────────────
 Work:                          Library:
   ☐ Today          5             ▸ Recent
   ⇣ Inbox          3             ▸ Pinned
   ◐ Projects                     Work
       Tessera     12               ▸ Projects
       Halenote     4               ▸ Areas
   ≡ Views                          ▸ Resources
       Due this week                ▸ Home
       Mine in Linear               Archive
   ↻ Review         2
```

- **Today** = tasks due/scheduled today plus service reminders and agent
  questions awaiting an answer. **Review** = GTD weekly review signals (stale
  projects, projects without open tasks).

Pros: the cleanest home for service-driven work (reminders, agent questions,
phone captures); each mode stays short; scales to many connectors.
Cons: a mode switch hides half the navigation at any time. Users lose "where
is this note" context while working, and that context is the strength of the
folder tree. Today and Review need the service, so offline the Work mode is
thin. It reshapes the sidebar Oleg approved on 2026-10-04 instead of
extending it. It risks the Notion/Tana outcome: navigation designed around
features rather than the vault.

## 5. Recommendation

**Option B, delivered in phases, with option C's Today/Review kept as
candidate *Views* rather than a mode.**

1. **Folder notes and project rows (option A).** A folder with `_index.md`
   opens it on selection. Project status glyph and nonzero open-task count.
   `_index.md`/`AGENTS.md` are not repeated as children. Reader-only; computed
   from the existing index; no service needed.
2. **Projects section.** Derived, cross-domain, grouped by status, with a
   divergence/invalid marker. Still reader-computable; the service serves the
   same projection to phone and agents.
3. **Views (service).** Saved filters over Markdown tasks and projects, stored
   in service state. Today and Review start here as built-in views; promote one
   to a fixed place only if daily use proves it.
4. **Connector annotations.** Explicit binding, provider status and owner
   beside Markdown status, divergence surfaced, last-observed time on hover.

Why B over A: A leaves the main question ("what is in flight across domains")
to the palette or to hand-written dashboards, which are the Obsidian-style
generated files the owner has ruled out. Why B over C: C pays a permanent
navigation cost (the mode switch) for features that do not exist yet. B can
grow into C's content without the switch.

## 6. Open questions for Oleg

1. **Status vocabulary.** Which `status:` values exist in the vault today, and
   should the glyph set be fixed (planned / active / paused / done) with
   anything else shown as "other"? (Measure on the vault before deciding.)
2. **Area folders.** Should an `Areas/*` folder with `_index.md` (`type: area`)
   get the same folder-note behaviour? Should Areas get a section, or stay in
   the tree only? (Proposed: folder note yes, section no.)
3. **Open-task count scope.** Count open checkboxes in the whole project folder
   or only in `_index.md`? (Proposed: whole folder, excluding `_` subfolders and
   the archive.)
4. **Done projects.** A project marked `done` but not yet moved to Archive:
   should it be shown in "Show N done", or flagged in Inbox/Review as "ready to
   archive"?
5. **Folder-note click.** Selecting a project row opens `_index.md`. Is that
   also right for plain folders with an `_index.md` (domain roots), or should
   plain folders keep select-only behaviour?
6. **Views authority.** Are Views per-vault app state on the desktop, or only
   service-owned (and so missing without the service)? Proposed: service-owned,
   with built-in Markdown-only views also computable locally.
7. **Divergence policy.** When a bound provider and `_index.md` disagree, is a
   quiet marker enough, or should it also appear in Inbox/Review as something
   to resolve?

## Evidence and limits

- App descriptions reflect the publicly documented navigation of each product
  as of this writing. Product behaviour was not re-measured for this document.
- The 272 `_index` occurrences come from the PRD §4 measurement, not from a
  fresh count. Phase 1 should start with a vault measurement of `type:` and
  `status:` values in `_index.md` files. That count is the positive control:
  it should find the known projects before an absence of any status is
  trusted.
