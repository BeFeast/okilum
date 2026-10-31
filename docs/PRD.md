# Okilum — Product Requirements

Approved at the promotion interview on 2026-09-02. This file is the canonical product
contract for the code. It supersedes the intake idea note, which stays as the origin
record and decision history.

**AI Brain POC extension (2026-09-05):** [scope and acceptance](ai-brain-poc.md)
and [versioned foundation contracts](ai-brain-contracts.md) define the approved
POC phase. The v0 reader below remains read-only; the POC explicitly adds source
editing, durable goal orchestration and connectors. An approved contract is not a
claim that these capabilities are implemented.

**AI Brain alpha extension (2026-09-05):** [one-project scope and acceptance](ai-brain-alpha.md)
define the next phase after the accepted POC: ordinary workspace integration, reusable
goals, saved connectors and exact export, evaluated on a separate halenote copy
under external T3 development control.

**Proposed next slice (issue166):** [persisted AI proposals](ai-brain-proposals.md)
records a P1 implementation contract and gated breakdown. It does not enable
provider jobs, canonical enrollment or deployment.

**Reader editing extension (2026-10-04, #354):** [source editing and safe save](reader-source-editing.md) adds explicit desktop source editing to ordinary notes. The v0 reader protocol remains read-only; this approved phase does not require Brain enrollment.

**Single-file viewer extension (2026-10-06, #560):** [quick viewer](single-file-viewer.md) defines document-only startup, lazy folder browsing and explicit vault upgrade.

**Vault window extension (2026-10-06, #551):** [vault windows](vault-windows.md) defines vault-window selection (Ask every time by default, configurable reuse or new window) and explicit duplicate windows sharing vault resources.

**Sync extension (2026-10-06, #574):** [explicit Sync enrollment](sync-enrollment.md) defines the approved opt-in background/reuse contract. Slice 1 tests compatibility only; it does not enable Sync in the reader.

**Typed note views extension (2026-10-07, #636):** [typed views and Tasks editing](typed-note-views.md) defines native views over plain Markdown, explicit revision-checked task edits and Undo. The reader/MCP protocol remains read-only.

**Remote Log view extension (2026-10-20, #1441):** [vault-scoped Kibana sources](remote-log-view.md)
adds explicit Discover import and bounded remote history in the existing Log view.
Credentials use OS storage; event data stays session-only. Live reconciliation reads the
whole selected window through a fresh PIT per cycle and confirms coverage only after
all pages finish; bounds stop explicitly as incomplete. Corporate/native live acceptance
and the visibility target remain unproven and cannot be silently substituted.

**Note aliases (2026-10-23, #1562):** [reserved frontmatter aliases](note-aliases.md)
adds vault-wide wiki resolution and Quick Open matching; declarations are plain
property chips, with existing canonical-path precedence and explicit ambiguity.

**Owned Settings/Feedback windows (2026-10-24, #1582):** [non-modal tools](owned-tool-windows.md)
defines owner-relative positioning, lifecycle, singleton reuse and Esc, with native
Windows ownership and explicit platform limitations.

**Document font settings (2026-10-29, #1836):** Settings → Appearance offers
system font families with previews for note text and code, applied immediately
to Reader, Live Preview, Source, code files and CSV tables. Choices persist in
application settings; unavailable fonts quietly use the embedded Noto Sans /
Cascadia Code defaults. Reset restores both defaults. UI chrome and the independent
color emoji fallback keep their existing typography.

**Core-first optional modules (#2091/#2092, 2026-10-31):**
[Settings → Features](optional-features.md) makes Agents, Second brain and Sync
opt-in, off by default, with immediate UI/lifecycle changes and no disabled-module
requests. The existing managed Workspace is the Second brain surface; Now remains
separate work in #1927.

## 1. What Okilum is

**Image import (#1743, 2026-10-29):** [WebP image paste and import](image-import.md)
defines app-wide conversion, quality and optional original retention, with
native disk/link/Undo acceptance before merge. JPEG XL remains separate.

**Project manager extension (2026-10-21, #1418 / #1415):**
[Project manager chat](project-manager.md) connects an area's explicit T3 binding
to the existing Reader window through `ManagerGateway`. Managers remain in T3;
credentials use OS storage and sends retain durable request identities. Executor
controls, scheduled-task model following and live-server acceptance remain separate.

**Project navigation extension (2026-10-22, #1472 / #1474):**
[Vault projects and application navigation](project-navigation.md) moves app
sections into the app menu, limits the sidebar to vault navigation, and defines
vault-wide project discovery, reversible scoping and the macOS manager webview.
Explicit manager creation retains launch intent and revision-aware registry writes.

**Project Board and compatible Area manifests (2026-10-27/29, #1767, #1859):**
[Native project Board](project-board.md) adds an explicitly bound read-only stage
projection in the center, related-note navigation and draft-only Manager context.
[TOML manifests](area-manifests.md) prefer TOML with absence-only legacy YAML
fallback. Invalid cards retain precise visible diagnostics beside healthy cards.
Delivery/human acceptance and execution mutations remain separate.

**Board management (2026-10-29, #1862):** [inline authoring](project-board.md#board-management-1862)
creates an empty `board.toml` explicitly and edits project cards and the selected
stage note. Existing boards retain their selected format, revision checks and
atomic writes; this does not convert vault manifests or dispatch executor work.

A local-first desktop knowledge environment that replaces Obsidian without taking
ownership of the files.

Product pillars, in the order they will be defended when they conflict:

1. Outstanding rendering of Markdown and rich media.
2. Search with the responsiveness and expressive power of an Elasticsearch-class system.
3. Deliberate cross-linking — wikilinks, backlinks, aliases, transclusion, with the
   graph as a derived view rather than a feature.
4. A transparent knowledge layer: raw → wiki → indexes → cited traversal. Vector and
   semantic retrieval only on measured need.
5. A safe multi-device story.

**Local project onboarding (#2033, path A, Oleg 2026-10-30):**
[Local project start](local-project-start.md) lets any ordinary vault folder,
including its root, become a local project and create an empty Board without
mandatory goal/repository/executor/stage. Local cards use a separate portable
TOML sidecar and explicit manual task state, independent of issue/build evidence.
Verified local T3 Manager binding (#1801) follows in slice 3 and is not claimed
by these local-start slices.

## 2. Canonical data

- Markdown and adjacent media files are canonical, inspectable, portable, and
  editable without Okilum.
- Search indexes, thumbnails, embeddings, graph projections, and caches are **derived**
  and must be rebuildable from the files alone.
- **v0 does not write to your notes.** Editing is external. This is a hard constraint,
  not a phase-one simplification, and it is load-bearing for §4.

## 3. v0 scope

Reader and searcher. Editing external, via any editor plus a file watcher.

Deferred with triggers, not dropped: plugins, Canvas, Bases, mobile, web clipping,
automation, semantic retrieval, accessibility on the GPUI stack, and the Depth-1 web
client over the frozen protocol.

## 4. Note identity and link resolution

**Decision (2026-09-02): identity is the path from the vault root.**

Resolution follows Obsidian's shortest-unique-path rule so existing vaults keep
working. Where Okilum departs from Obsidian: **an ambiguous link is surfaced as
ambiguous.** It is not silently resolved to an arbitrary winner.

The vault this was decided against has 3933 notes, 580 of them (14.6%) sharing a
filename stem with another note across 67 colliding stems — `_index` alone
appears 272 times.

Measured by running both resolvers over all **11 742** wikilink occurrences
actually written in that vault:

| Outcome | Occurrences | |
|---|---|---|
| Same note under both rules | 9 673 | 82.4% |
| Unresolved under both (the link names nothing) | 1 633 | 13.9% |
| **Old opened a different note — it was wrong** | **91** | 0.8% |
| **Old silently picked one of several candidates** | **199** | 1.7% |
| Old opened something for a broken link; now honestly unresolved | 21 | 0.2% |

Total resolved went **up**, 9 752 → 9 889, because fixing qualified-path
resolution recovers more links than surfacing ambiguity costs.

An earlier estimate of "742 ambiguous occurrences" appears in the promotion
notes. That was a cruder proxy — every bare-stem link whose stem collides with
anything — and it overstates the real figure by 3.7×: an exact full path is
unambiguous however many notes end with the same segments, and a qualified link
disambiguates itself. The measured number is 199.

Rejected alternatives, and why:

- **Frontmatter UUID as canonical id.** The most robust answer to renames, moves, and
  sync — and it requires Okilum to write into the user's files, which §2 forbids in
  v0. Revisit if and when v0's read-only constraint is lifted.
- **Okilum-owned sidecar identity database.** Survives renames without touching
  files, but introduces authoritative state that can drift from disk and does not
  travel between devices. Rejected as a worse trade than honest path identity.

A **wikilink** is a **suffix of a path**, and it resolves exactly when it names one note
and no other. `[[alpha]]`, `[[notes/alpha]]` and `[[a/b/notes/alpha]]` are
progressively more qualified names for the same note. An exact full path from the
root always wins, since paths are unique. A target starting with `./` or `../` is
resolved against the directory of the note that wrote it — those links say nothing
on their own, and resolving them by suffix would answer with an unrelated note
that happens to end the same way.

Ordinary Markdown document links use **source-relative first** semantics: from
`notes/start.md`, `[Sibling](sibling.md)` chooses `notes/sibling.md` even if
`sibling.md` also exists at the root. Explicit `./` and `../` never use suffix
fallback. Only an absent relative candidate permits exact vault-root, then
unique-suffix compatibility lookup; existing invalid/unreadable entries do not
permit redirection. A leading slash means vault-root, except absolute filesystem paths inside the
open vault normalize to vault-relative identity (#486). Existing outside-vault
files offer explicit Reveal/Copy actions without opening automatically. Obsidian
inline destinations with spaces are accepted when they resolve to existing files.
Wiki shortest-unique/root-exact semantics and aliases remain unchanged.

Document links retain their heading intent across Reader, managed rendered
preview and explicit Source/Live Preview Open link. A missing, duplicate,
unsupported or stale heading cannot silently become a successful note-top
navigation. Reader retains Setext heading support; a limited managed surface
reports its unsupported syntax explicitly. Reader lands block references
(`#^id`, #651) under the same rules; extensionless Markdown inference and
attachment actions remain separate capabilities.
See [the document link contract](document-links.md) for parsing and acceptance.

Consequence accepted: a rename breaks links exactly as it does today. Rename tracking
is a separate, later problem and must not be smuggled in as a sidecar.

## 5. Rendering

The target is not "displays some Markdown". The acceptance corpus must cover:

- CommonMark and GFM fundamentals;
- Obsidian wikilinks, embeds/transclusion, callouts, tags, aliases, frontmatter;
- tables, task lists, footnotes, block quotes, nested lists, fenced code with syntax
  highlighting, math, diagrams;
- local and remote images, animated images, SVG, audio, video, PDF;
- links to local files, and safe embedded HTML where deliberately supported;
- large documents and media, incremental updates, selection and copy, find-in-note,
  scrolling, zoom, high-DPI;
- clear fallback for unsupported or unsafe content, without painting diagnostics over
  the document.

Judged on visual quality, correctness, latency, scroll smoothness, memory, CPU/GPU
use, and input behaviour — not appearance alone.

**First acceptance corpus = Oleg's vault, measured 2026-09-05 (3989 notes).** Feature
frequency decides order; anything at zero waits for a trigger:

| Feature | Notes | Status |
|---|---|---|
| tables | 1196 | rendered |
| wikilink aliases `[[a\|b]]` | 980 | rendered |
| task lists | 293 (2699 boxes) | parsed, checkbox rendering to verify |
| callouts `> [!type]` | 111 (warning 79, note 31, info 30, tip 29, success 28, important 20, summary 14, danger 14) | literal, **#1 gap** |
| strikethrough | 87 | rendered |
| math `$…$` / `$$` | 79 real (613 raw `$` hits are shell and prices) | code-styled fallback |
| highlight `==x==` | 60 | not rendered |
| heading links `[[a#h]]` | 50 | resolve, no scroll-to |
| note transclusion `![[note]]` | 37 | not rendered |
| PDF links | 24 | plain link |
| mermaid | 20 | [rendered diagrams](mermaid.md) in Reader and Live Preview with merman 0.9.2; highlighted-code fallback on failure; Source stays code. Oleg accepted 28.10; real-corpus measurement before Stable ([#1182](https://git.oklabs.uk/BeFeast/okilum/issues/1182)) |
| html tags | 16 | passed through |
| footnotes | 10 | rendered with back-links ([#651](obsidian-syntax.md)) |
| block refs `^id` | 0 | hidden, links land, block embeds ([#651](obsidian-syntax.md)) |
| audio | 0 | wait |

## 6. Search

Target capabilities: near-instant incremental full-text search; phrase, field, tag,
path, date and Boolean filtering; typo tolerance, stemming, aliases, multilingual
text; deterministic ranking that can explain a match; highlighted snippets and fast
jump-to-match; note, heading, block, frontmatter, link and media-metadata indexing;
incremental watcher updates plus a full rebuild path; optional semantic retrieval that
complements rather than obscures lexical results.

**Decision (2026-09-02): the engine is still not chosen.** `tantivy` is the incumbent
and indexes the 3874-note corpus today, but it was a spike-level pick. The choice is
gated behind corpus and latency acceptance tests, written **before** any engine is
measured against them — the same pre-registered-band discipline used for the framework
probes. `tantivy` stays in place until a verdict exists.

## 7. Framework

**Decision (2026-09-02): GPUI, via `gpui-component` (upstream renamed to `longbridge/gpui-kit` 2026-09-04).** The other three candidates were
eliminated on evidence:

| Candidate | Outcome |
|---|---|
| Qt | Out. Typography judged worst of four; real quality work means leaving `QTextDocument`, making a custom C++ document engine the most expensive path available. |
| Slint | Out. Failed the char-precise selection gate — a row-granular ceiling, proven by probe. |
| Electron | Out of v0 on measured performance: 400–423 MB memory (band: fail ≥300) and 1262 ms first paint (band: fail ≥1000). Retained as the Depth-1 web-client path over the frozen protocol. |
| **GPUI** | **Selected.** Best-of-four cold start (243 ms), no memory regression, and all gating defects repaired. |

**Go confirmed by Oleg 2026-09-05, after two days of daily use on the live vault:**
performance judged excellent, rendering support to be built out (§5 corpus below),
and anything the stack cannot do gets fixed in the vendor and upstreamed. Other
frameworks stay available for isolated surfaces (a separate window) if a case ever
demands it; none is planned. The four measured-open defects from the spike are closed
and three of the fixes are merged upstream (2945/2946/2947); the framework gate is
**closed**, not pending.

Fork posture, binding:

- Double-pin: vendored clone plus `Cargo.lock`.
- Weekly rebase of a deliberately small patch set (Oleg, 2026-09-04: monthly was
  the original posture; the first bump after four days already needed one manual
  conflict resolution, and upstream is active, so the interval is a week).
- **Every vendor patch is upstreamed to `longbridge/gpui-kit`.**
- `gpui-ce` is the tracked exit ramp.
- Accessibility is absent on this stack. Deferred and recorded; not a v0 gate.

Repair cost, measured by Probe 2 (2026-09-02): four open defects closed for **59
insertions / 3 deletions across 3 files**, all under `crates/base`, with **gpui core
untouched** and 726 crate tests passing. Two of the four turned out not to be product
defects at all — one did not reproduce under a corrected test harness, and the frame
pacing was proven pre-existing by an unpatched baseline on the same hardware.

## 8. Constraints

- **No-compromise quality and performance.** Never traded for less development effort.
  A library that fails an acceptance gate is replaced, or the component is built from
  scratch. Gates do not move. Development cost is a tiebreaker between passing
  candidates and nothing more.
- Runtime target: Arch-based Linux first (Omarchy, CachyOS). macOS and Windows later
  by design.
- Derived data must always be rebuildable from the canonical files.
- **Idle means idle.** With no note changing on disk the process must sit at 0% CPU
  and touch nothing under the index. Anything the reader does to the vault (reading
  every note for backlinks, a rebuild) must never register as a change to itself.
  Broken once on the first POC launch (#19: read events counted as writes, index
  rebuilt every 3–6 s at 300% CPU); the watcher test suite now reads a whole vault
  after the watcher is up and asserts silence.

## 9. Non-goals for v0

A Markdown editor. Plugin API. Canvas. Bases. Mobile. Web clipping. Vector search.
Cloud sync service. Any feature whose absence is not currently costing something
measurable.

## 10. Still open

- Sync authority, conflict semantics, encryption, and backup/history model.
- The maximum corpus, document, and media scale, and the performance bounds that go
  with them.
- Which Obsidian dialect features define the first acceptance corpus.
- Whether agent features run in-process, via local CLI, or through the sidecar.
- Rename tracking (see §4).

## 11. AI Brain POC

Okilum carries a thought through task, execution and durable knowledge, removing
manual context transfer between systems. It owns the goal and overall plan while
execution engines own their delegated stages. The selected vertical is inbox →
native conversation → task connector → T3 stage → Markdown outcome/evidence →
notification. [The POC specification](ai-brain-poc.md) defines its scope and the
broader requirements whose sequencing remains open.

The writing prohibition in §2 continues to govern v0, not this explicitly approved
POC phase. Markdown and adjacent media remain canonical; Todoist remains task
authority during transition. Operational execution state is separate and durable,
not a reconstructible search cache. The new record identities do not migrate v0
path-based notes. Exact source access and revision-aware writes are governed by
[AI Brain contracts v1](ai-brain-contracts.md), separately from the existing reader
wire protocol.

## 12. AI Brain indexed retrieval and reviewed context

The next additive AI Brain milestone supplies automatic rebuildable Markdown
indexing, lexical and measured embedding-based retrieval, reviewable bounded
context consumed by T3, and AI context export with original-source provenance.
[Indexed retrieval and reviewed context](ai-brain-retrieval-context.md) defines
the authority, freshness, goal-isolation and acceptance boundaries. This extends
the explicitly approved AI Brain phase; vector search remains outside v0.
