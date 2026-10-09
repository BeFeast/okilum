# Research: Excalidraw round trip — edit the actual drawing (#478)

Status: research only. No product code changes. This document informs a
go/no-go decision and a slice plan for #478.

Problem: the Reader renders Excalidraw drawings natively (#445). Its "Open in
Excalidraw" button copies the scene to the clipboard and opens the owner's
self-hosted editor, `https://draw.oklabs.uk`. That editor is the stock
`excalidraw/excalidraw` image with no storage backend, so it opens a blank (or
the last local) canvas. Even if the user pastes the scene, edits never reach the
vault file.

## TL;DR

- **The stock editor can only get a scene in one way, and nothing comes back.**
  The self-hosted app accepts a scene only from its own `localStorage`, a
  `#json=` share link (needs the excalidraw.com-style backend), a `#url=` link
  it `fetch`es, or a manual paste or file open. It has no postMessage API for
  third parties and no way to save back other than "Save to disk". So no
  URL-hash or postMessage trick turns it into a round trip.
- **A homelab storage backend is not needed and would make things worse.** It
  would hold a second copy of the drawing outside the vault. That copy would
  need its own sync back to the file, against the "canonical files" rule.
- **Recommendation: a Tessera-local bridge.**
  - Tessera serves a small editor page from `127.0.0.1`. The page bundles the
    MIT `@excalidraw/excalidraw` package, and Tessera hands it the scene.
  - On Save, the page sends the scene back. Tessera writes it into the vault
    file with the existing revision-checked, atomic `FileEditor` save path.
  - The hard part is not the transport. It is **writing the `.excalidraw.md`
    file without corrupting it.** Upstream Excalidraw deletes the plugin's
    `rawText` field on load. The Obsidian plugin treats the Markdown
    `## Text Elements` section as authoritative over the JSON. So a naive
    "replace the Drawing block" write silently reverts every text edit the
    next time Obsidian opens the file. The writer must rebuild those sections
    for the elements that changed, and only those.
- **In-app editing in GPUI is feasible only as a long project.** Upstream's
  element core alone is about 37.6k lines of TypeScript. A useful subset is
  about 6–10 weeks of work. Parity takes many months. Defer it until the
  bridge is in daily use.
- **Estimate for the bridge MVP: about 3–3.5 weeks across 5 slices.** Inserting
  images from the editor into the vault adds about 0.5 week.

## 1. Where Excalidraw lives in this repo today

| Concern | File | Notes |
|---|---|---|
| Scene parsing: plain `.excalidraw`, `.excalidraw.md` with `json` or `compressed-json`, `## Embedded Files` | `crates/tessera-core/src/excalidraw/mod.rs` (`Scene::parse`) | Bounded (32 MiB source, 10k elements, 20k points). Keeps the exact scene in `Scene::json` and records which files are images. Does not read `## Text Elements` or `## Element Links`. |
| LZ-string Base64 **decoder** | `crates/tessera-core/src/excalidraw/lz.rs` | Decode only. A round trip needs an encoder (§4.4). |
| Scene → SVG display list (roughr) | `crates/tessera-core/src/excalidraw/vector.rs` (`Scene::vectors`) | Text, shapes, arrowheads, freedraw, images. Unsupported kinds produce a warning. |
| Embed detection in notes | `crates/tessera-core/src/render.rs` (`is_drawing` call sites) | `![[x.excalidraw|300]]` sizing. |
| Load, image inlining, resvg raster, LRU caches, **Open in Excalidraw** button | `crates/tessera-shell/src/reader_drawing.rs` | `load_document` (≈ line 143) inlines vault images as `data:` URLs for rendering only. The button (≈ lines 350–366) copies `Document::json` and calls `cx.open_url("https://draw.oklabs.uk")`. |
| Routing drawings to the drawing view | `crates/tessera-shell/src/main.rs` (≈ 863, 1597, 1742), `reader_files.rs` (≈ 309), `reader_thumbnail.rs` (≈ 41) | |
| Revision-aware atomic note save | `crates/tessera-core/src/file_editor.rs` (`FileEditor::open/save/keep_mine/refresh_from_disk`) | Byte-compare against base, atomic exchange, displaced-inode retention, note history pre-image, crash-safe drafts. Unix only (`rustix`, `os::unix`). |
| Contracts | `docs/excalidraw-reader.md`, `docs/reader-source-editing.md`, `docs/design/reader.md` ("The deferred local Excalidraw editor needs no editor-URL setting") | The design doc already assumes a *local* editor. |

## 2. Evidence base

Sources were read at these revisions. All clones and probes are outside the
repo, in a scratch directory.

- `excalidraw/excalidraw` at `53973c3` (2026-10-06). This is the app the
  self-hosted Docker image builds: the `Dockerfile` runs
  `yarn build:app:docker` and copies `excalidraw-app/build` into nginx.
- `@excalidraw/excalidraw` **0.18.1** (npm `latest`), unpacked.
- `zsviczian/obsidian-excalidraw-plugin` at `f30b4c5` (2026-10-03). It depends
  on the fork `@zsviczian/excalidraw` 0.18.140, not upstream.

### 2.1 What the stock web app accepts (`excalidraw-app/App.tsx`, `initializeScene`)

- `#json=<id>,<key>`: fetches an encrypted scene from the share backend
  (`importFromBackend`). The owner's image has no backend, so this is not
  available.
- `#room=…`: live collaboration. Needs `excalidraw-room` plus storage.
- **`#url=<encoded URL>`** (≈ lines 231, 304–327): `fetch`es any URL and
  `loadFromBlob`s it, after a confirm dialog if the local canvas is not empty.
  This is the only way to preload a scene into the stock app without a backend.
  It is one-way:
  - The drawing then lives in the app's `localStorage` on `draw.oklabs.uk`.
  - The page is HTTPS, so loading from Tessera's loopback needs CORS headers
    from Tessera.
  - Chromium's Local Network Access prompt applies when a public origin calls
    loopback. Safari's mixed-content handling of `http://127.0.0.1` from an
    HTTPS page is **unverified**.
- postMessage: only `ExcalidrawPlusIframeExport.tsx` listens. It answers
  `excalidraw-plus` origins with a JWT signed by Excalidraw+, and it only
  *exports* `localStorage`. There is no inbound "load this scene" message.
- Paste: `clipboard.ts:79` accepts a full `{"type":"excalidraw",…}` scene. So
  today's clipboard hand-off works, but only if the user presses ⌘V/Ctrl+V on
  the blank canvas. That matches the owner's report.
- Output: download a file, the File System Access "save to current file"
  (Chromium, `data/json.ts` `fileHandle`), copy, export image. **Nothing reaches
  the vault without the user saving a file and Tessera importing it.**

Conclusion: URL hash and postMessage on the stock app give at best "open a copy".
They never give a round trip.

### 2.2 The npm package API

`@excalidraw/excalidraw` exports:

- the `<Excalidraw>` component, with `initialData`, `onChange` and
  `excalidrawAPI` props;
- `serializeAsJSON`, `restore*`, `reconcileElements`, `loadFromBlob`,
  `getDataURL` and `CaptureUpdateAction`.

That is everything a host page needs to load a scene, observe edits and
serialize the result.

The package is MIT. React 17/18/19 is a peer dependency, and **all other
dependencies are external** (roughjs, jotai, radix, …). So the page needs a
bundler.

### 2.3 Probe: a loopback page bundling the package (scratch, not committed)

The probe has three parts:

- An 11-line `entry.jsx` that mounts `<Excalidraw initialData=… excalidrawAPI=…>`.
- A bundle built with esbuild 0.25 (`--bundle --minify --splitting --conditions=production`).
- A Node static server bound to `127.0.0.1:0`, driven by headless Chromium
  through playwright-core.

The input was Tessera's own fixture
`crates/tessera-core/tests/fixtures/excalidraw/elements.excalidraw`. The probe
added an Obsidian-style `rawText`, a `link: "[[Some Note]]"`, `customData` on
every element, and a top-level unknown key. The fixture also has an image
element whose `fileId` has no entry in `files`, like a vault-backed image in an
`.excalidraw.md` file.

Results:

| Measure | Result |
|---|---|
| Bundle | `entry.js` 806 KB minified (251 KB gzip) and `entry.css` 145 KB. Including lazy chunks (locales, mermaid, subsetting worker), 8.5 MB in 184 files. Fonts (`dist/prod/fonts`) add **14 MB**. Upper bound ≈ 23 MB before trimming locales and CJK fonts. |
| Positive control | 9 of 9 elements loaded and no page errors. Load to API-ready took 319 ms on this container. That timing is not portable: compare on one machine. |
| Transitive licences (npm, incl. dev tools) | MIT 184, ISC 39, Apache-2.0 11, BSD-3 7, (MPL-2.0 OR Apache-2.0) 1, CC0 1, Unlicense 1, 0BSD 1, (MIT AND Zlib) 1. Fonts are OFL/MIT; `web/inbox` already ships OFL fonts as a precedent. |
| `rawText` on a text element | **Dropped on load.** `packages/excalidraw/data/restore.ts:534` has `delete (element as any).rawText` ("cleanup legacy obsidian-excalidraw attribute"). |
| `link: "[[Some Note]]"` | Preserved, also after an edit. |
| `customData` | Preserved on every element type. |
| Image with no file in `files` | Preserved (`status: "pending"`, `fileId` kept). It renders as a placeholder. |
| Unknown element keys | Kept (restore spreads `...element`), except `rawText`. |
| Unknown top-level keys | Dropped. `serializeAsJSON` emits only `type, version, source, elements, appState, files`. |
| Load-time normalisation ("churn") | Restore fills defaults (`index`, `roundness`, `boundElements`, `updated`, `lineHeight`, `autoResize`, …) and rewrites `version`/`versionNonce` on a minimal fixture. Real plugin files already carry most of these fields. Either way, **"changed" must be judged against the restored baseline, not the file bytes.** |

### 2.4 The Obsidian plugin's `.excalidraw.md` format

Sources: `src/shared/ExcalidrawData.ts`, `src/shared/excalidrawMarkdownParsing.ts`,
`src/utils/sceneDataUtils.ts`.

Layout written by `generateMDBase` / `generateMDSync`:

```
---
excalidraw-plugin: parsed        # or raw
…other frontmatter…
---
…"back of the note": arbitrary user Markdown…
%%                               # present unless the text section is visible
# Excalidraw Data

## Text Elements
<raw text with [[links]]> ^<8-char element id>

## Element Links                 # only if any
<id>: <link>

## Embedded Files                # only if any
<fileId>: [[path/to/image.png]] <optional colorMap JSON>
<fileId>: $$<latex>$$
<fileId>: https://…
%%
## Drawing
```compressed-json             # or ```json
<LZString.compressToBase64(JSON), split every 256 chars, joined by a blank line>
```
%%
```

The rules that matter for writing:

1. **Markdown wins over JSON.** `ExcalidrawData.ts:911`: "The Markdown # Text
   Elements take priority over the JSON text elements". On load, every
   `<text> ^id` entry overwrites `rawText` and re-derives `text` (parsed mode).
   `## Element Links` overwrites `link`.
   - Consequence: if Tessera rewrites only the Drawing block, Obsidian silently
     reverts every text edit made in the browser.
2. **Three text fields** (header comment in `ExcalidrawData.ts`):
   - `rawText`: Markdown source, no wrap breaks.
   - `originalText`: no wrap breaks; parsed or raw depending on mode.
   - `text`: displayed text, with wrap breaks.

   In `parsed` mode, `[[Note|alias]]` is shown as `alias`. Upstream knows
   nothing of `rawText` and deletes it (§2.3).
3. **Compression:** `LZString.compressToBase64`, chunked at 256 characters with
   `\n\n` separators (`sceneDataUtils.ts:24`). The decoder strips CR/LF.
   Tessera's decoder already skips whitespace, so it accepts this.
4. **Deleted elements are written** (`elements.concat(deletedElements)`). The
   plugin's incremental sync (below) uses them to remove elements from an open
   view.
5. **Images live outside the JSON.** For vault images, `files` in the scene is
   usually empty. The image comes from `## Embedded Files`. LaTeX, Mermaid
   (`customData`), Markdown/PDF embeds and hyperlinks are also there.
   **Upstream cannot render any of these**; they appear as placeholders.
6. **Concurrent edits.** When an open drawing is modified on disk,
   `FileManager.modifyEventHandler` → `getDrawingModifyRoute`
   (`core/managers/fileModifyRouting.ts:26`) does an **incremental sync** for
   `.md` drawings. The exception is a full reload when the view was idle for
   5 minutes. `ExcalidrawView.synchronizeWithData` (≈ line 3875) takes the
   incoming element when its `version` is higher, and on a tie "the incoming
   version will be honored". Deleted IDs are removed.
   - Consequence: an Obsidian window open on the same drawing merges Tessera's
     write correctly only if every changed element has a bumped `version`.
     Upstream bumps it on edit. And deletions must be written as
     `isDeleted: true`, not dropped.

## 3. Options

### A. Keep the stock editor; pass the scene by URL hash (`#url=`)

Tessera serves the plain JSON on loopback with CORS for `draw.oklabs.uk`, then
opens `https://draw.oklabs.uk/#url=http%3A%2F%2F127.0.0.1%3A…%2Fscene%2F<token>`.

- Cost: about 3 days.
- Data safety:
  - Read-only, so the vault is safe.
  - But the drawing now sits in a browser `localStorage` copy that diverges
    silently.
  - Loading replaces the owner's existing local canvas, after a confirm dialog.
- Browser risk: Local Network Access prompts, Safari mixed content (unverified),
  and a CORS endpoint any page can probe.
- **Not a round trip.** It fixes "blank canvas" only.

### B. postMessage into the stock editor

Not possible: no listener exists for third-party origins (§2.1). It would need
a fork of the app, which is option D.

### C. Tessera-local bridge: loopback page hosting `@excalidraw/excalidraw` — **recommended**

How it works:

- Tessera ships a prebuilt static page. Tessera-shell serves it on
  `127.0.0.1:<random>` and opens `/edit/<256-bit token>` in the default browser
  (`cx.open_url`, as today).
- `GET /scene/<token>` returns the scene. Vault images are inlined into `files`
  as `data:` URLs, reusing `load_document`'s bounded, ambiguity-aware resolver.
- **Save** (⌘S/Ctrl+S, plus a debounced autosave matching the note editor's
  policy) `PUT`s the serialized scene back.
- Tessera turns that into an exact file update (§4) and commits it through
  `FileEditor`. That gives the byte-compare against the opened base, atomic
  exchange, history pre-image and crash-safe draft.
- On conflict, Tessera returns 409 and the page shows inline actions: Reload /
  Keep mine / Save as copy. These match `docs/reader-source-editing.md`, which
  writes no automatic merge into notes.

The Reader watcher already invalidates the decoded scene, so the native render
updates by itself.

- **No homelab component, no CORS, no Local Network Access prompt.** The page
  and the API share one loopback origin. It works offline. The owner's
  `draw.oklabs.uk` is untouched and can be retired or kept for other use.
- Security: these rules make the bridge acceptable.
  - Bind to `127.0.0.1` only.
  - Unguessable per-session token in the path.
  - Reject any `Host` other than `127.0.0.1:<port>` (DNS rebinding).
  - Reject a `PUT` whose `Origin` is not the page's own origin.
  - No CORS headers.
  - Body bounded by `MAX_SOURCE_BYTES`.
  - Each token is bound to one canonical vault path. No directory listing.
    Strict CSP.
  - The session ends when the Reader closes the drawing or Tessera quits.
- Cost: see the slice plan, about 3–3.5 weeks.
  - New build-time toolchain: a pinned `package.json` and lockfile, built with
    esbuild. Node is already present in the Forgejo CI and release images.
  - About 10–23 MB of assets in the app bundle, depending on font and locale
    trimming.
  - A small HTTP surface in tessera-shell.
- Data safety: high, *given* §4. Every write goes through the existing
  revision-checked save, so a concurrent Obsidian or sync change produces a
  conflict, never a silent overwrite.

### D. A custom editor page hosted on the homelab, talking to a Tessera loopback API

The same page as C, but served from `draw.oklabs.uk`. It has all of A's
cross-origin problems (CORS, Local Network Access, mixed content), adds a
deploy step and an online dependency, and gains nothing over C.
Not recommended.

### E. Homelab storage backend (share or collab server)

This stores scenes in the backend's database. The vault file would then need a
sync daemon and a merge, which creates a second source of truth outside
canonical files and breaks the "derived data must be rebuildable" principle.
Only useful for editing from devices without Tessera. Out of scope.

### F. Embedded webview (e.g. wry) instead of the system browser

This keeps the editor "inside" Tessera, but:

- GPUI has no webview element.
- A separate wry/tao window competes with GPUI for the main-thread event loop
  on macOS. This is **unverified** and a real risk.
- It adds WebKitGTK/WebView2 runtime dependencies.

Revisit only after C ships, as a different way to present the same page and
the same write path.

### G. Native in-app editing in GPUI

See §5. It is feasible as a long project, not as the fix for #478.

### Summary

| Option | Round trip | Homelab change | Est. cost | Data-safety risk |
|---|---|---|---|---|
| A `#url=` to stock app | No (copy only) | None (CORS on Tessera) | ~3 d | Divergent browser copy; overwrites the local canvas |
| B postMessage | — | Fork needed | — | — |
| **C loopback bridge** | **Yes** | **None** | **~3–3.5 wk** | **Low with §4 writer + `FileEditor`** |
| D homelab page + loopback API | Yes | Deploy | ~4 wk | Same as C plus cross-origin attack surface |
| E storage backend | Indirect | Backend + sync | 4+ wk | Second source of truth |
| F embedded webview | Yes | None | C + 2–3 wk | Same as C; event-loop risk |
| G native GPUI editor | Yes | None | 6–10 wk subset; months for parity | Same writer as C |

## 4. Writing edits back without corrupting the file

The writer belongs in `tessera-core` (`excalidraw::write`, a pure function from
bytes to bytes) so it is testable without GPUI. The editor delivers a serialized
scene. The writer receives:

- the **opened bytes** (base);
- the **restored baseline** the page reports when it first loads (to neutralise
  §2.3 churn);
- the **edited scene**.

### 4.1 Element-level diff, not wholesale replacement

For each element `id`:

| State | What the writer emits |
|---|---|
| Unchanged versus the restored baseline | The **original JSON object from the file, byte-for-byte as parsed**. This keeps `rawText` and all fork-only fields. |
| Changed | The editor's element. Restore `rawText` (§4.2). Keep `version` as bumped by the editor. |
| New | The editor's element. |
| Deleted in the editor | The editor's tombstone (`isDeleted: true`). |
| Already a tombstone in the file | Kept. |

- `appState`: start from the original and take only the fields the user can
  change in the editor (e.g. `viewBackgroundColor`, grid).
- Unknown top-level keys of the original are carried over (`serializeAsJSON`
  drops them).
- **No-op save:** if nothing changed, write nothing. Tests must also show that
  a real edit does produce a write; that is the positive control the evidence
  rules require.

### 4.2 Text elements, `rawText` and `## Text Elements`

For each text element whose `originalText` changed:

- set `rawText := originalText`;
- rewrite its `<raw> ^id` entry in `## Text Elements`, using the plugin's
  format: `"{raw} ^{id}\n\n"`.
- New text elements get an entry. Deleted ones lose theirs.
- Unchanged elements keep their entry bytes exactly.

Links inside edited text are a problem. In `parsed` mode the browser shows
`alias` for `[[Note|alias]]`, so editing that text would flatten the link.
Mitigation: when the page loads a `parsed` drawing, show text elements whose
`rawText` differs from `originalText` **in raw form** (the same as the plugin's
`raw` mode). Editing then preserves the Markdown, and Obsidian re-parses it on
open.

Cost of that mitigation: until Obsidian re-saves the file, Tessera's native
render of an *edited* linked text shows the raw `[[…]]`. This is an owner
decision (§7, question 2).

### 4.3 Links and embedded files

- `## Element Links`: rewrite the entry only for elements whose `link` changed.
- `## Embedded Files`: **keep verbatim.**
  - Strip from `files` every entry Tessera inlined for display (IDs listed in
    `## Embedded Files`). Vault images must never be written into the JSON as
    base64.
  - Images newly pasted in the browser stay as `data:` entries in `files` in
    the MVP. Tessera's reader already renders those (`load_document`). Whether
    the plugin migrates them into an attachment is **unverified**.
  - Slice 6 moves new images into the vault attachment folder and adds a
    `[[…]]` line instead.
- LaTeX, Mermaid, Markdown/PDF embeds: the browser shows placeholders. These
  elements are preserved untouched unless the user moves or deletes them;
  moving changes only geometry.

### 4.4 Re-encoding and splicing

- **Splice, don't regenerate.** Bytes before `# Excalidraw Data` (frontmatter,
  BOM, back-of-note Markdown) and after the closing `%%` are copied exactly.
  Line endings follow the file. Only the Text Elements, Element Links and
  Drawing blocks are replaced.
- Keep the file's encoding choice: `json` stays `json`, `compressed-json`
  stays compressed, with 256-character chunks and `\n\n`.
- Add an **LZ-string `compressToBase64` encoder** next to `lz.rs`.
  - Options: about 80 lines of our own, or the `lz-str` crate. The crate's
    licence and maintenance need an `about.toml` review first.
  - Tests: Tessera decode(encode(x)) == x, plus fixtures produced by JS
    `lz-string`. Byte parity with JS is not required; decodability by the
    plugin is.
- Plain `.excalidraw` files: emit the whole JSON with 2-space indentation, as
  excalidraw.com does, keeping unknown top-level keys.

### 4.5 Concurrency

- **Tessera ↔ external writers (Obsidian, Sync, git):** `FileEditor`
  byte-compares current disk against the opened base and refuses on mismatch.
  The page shows the conflict inline. There is no automatic merge, per
  `reader-source-editing.md`.
  - An element-level three-way merge would be possible (§2.4 item 6 is exactly
    that algorithm). But it changes an approved contract, so it is listed as a
    future option for the owner, not part of the MVP.
- **Obsidian open on the same drawing while Tessera saves:** the plugin's
  incremental sync merges by `version`. Correct `version` bumps and tombstones
  are therefore required. If the Obsidian view is dirty, it autosaves later.
  Its merged state includes Tessera's elements, so nothing is lost, but
  Tessera's next save will see a conflict. Acceptable and visible.
- **Two browser tabs or two Tessera windows on one drawing:** the
  `FileEditor` lock already refuses a second editor for one canonical path.
  A second Edit opens the existing session's tab.
- Windows: `FileEditor` is Unix-only. Drawing editing follows whatever the
  Windows note editor supports (open question).

## 5. Longer term: native editing in GPUI

What Tessera already has:

- the parser;
- roughr geometry with per-element seeds;
- bundled Excalidraw fonts;
- a resvg raster path;
- the §4 writer, which a native editor would reuse unchanged.

What an editor needs on top:

- **Interactive rendering:** today a drag would mean re-rasterizing up to
  4096 px through resvg per frame. Editing needs GPUI `paint_path` vector
  painting of roughr ops, or a cached-raster-plus-overlay strategy.
- **Hit testing and transforms** for rotated shapes, curves, freedraw and
  bound text; selection handles; multi-select; groups and frames.
- **Arrow binding and elbow-arrow routing**: upstream `binding.ts` has 3,234
  lines and `elbowArrow.ts` 2,304.
- **Text editing with Excalidraw-compatible wrapping and metrics**
  (`textWrapping.ts`, 740 lines), so Obsidian and the web show the same
  layout.
- Undo/redo, fractional `index` ordering, clipboard interop with
  `excalidraw/clipboard` JSON.

Size reference: upstream `packages/element/src` is 37.6k lines and `actions`
8.9k; `components/App.tsx` alone is 12k.

Estimate:

- A useful subset is about **6–10 weeks**: select, move, resize, delete and
  edit text of existing elements, plus add rectangle, ellipse, arrow and text
  without binding.
- Parity is many months.
- One extra risk: roughr's RNG differs from Rough.js. That is harmless for
  saved data, because Obsidian re-renders from seeds. But Tessera would draw
  slightly differently while editing than the web does.

Verdict: feasible, worth reconsidering once the bridge shows how much the owner
edits. The writer in §4 is the shared foundation either way.

## 6. Slice plan (option C)

| # | Slice | Scope | Est. |
|---|---|---|---|
| 1 | Core writer | `tessera-core::excalidraw::write`: element diff, `rawText`/Text Elements/Element Links reconciliation, `files` stripping, splice, LZ encoder. Fixture tests: no-op = zero writes (with a positive-control edit), compressed and plain, CRLF, BOM, back-of-note, tombstones, links, embedded images, LaTeX placeholders kept. | 4–5 d |
| 2 | Plugin compatibility control | Run the plugin's pure parsing helpers (`excalidrawMarkdownParsing.ts`, text-section regexes) under Node in CI against slice-1 output, plus JS `lz-string` decode. The owner opens the seven reference drawings, edited by Tessera, in Obsidian. | 2 d |
| 3 | Editor page | `web/excalidraw-editor/`: pinned React and `@excalidraw/excalidraw`, esbuild, self-hosted fonts and assets, English only, collab/share/remote libraries off. Save, debounced autosave, conflict UI inline, `beforeunload` guard. Reproducible build check in CI; licence notices. | 3–4 d |
| 4 | Loopback bridge | tessera-shell: minimal HTTP on `127.0.0.1:0` (std `TcpListener` or a small permissive crate). Token, Host and Origin checks, size bounds, session ↔ `FileEditor` per drawing, image inlining reuse, 409 conflict flow, Reader toast "Drawing saved" with Undo through note history. | 5–6 d |
| 5 | Reader UI | Replace the clipboard plus `draw.oklabs.uk` action with a glyph "Edit drawing" button (same 28px ghost button). Move "Copy scene" into More. Wording per platform. | 1–2 d |
| 6 | (optional) New images to vault | Pasted images saved to the vault attachment folder plus a `## Embedded Files` line, instead of inline `data:` URLs. | 2–3 d |

Slice 2 decision (manager, 2026-10-09): the plugin is AGPL-3.0, so it is a test-only oracle. CI fetches it at a pinned commit (sha256-verified); it is never committed, packaged or distributed. The non-required `excalidraw-compat` job runs it against writer output; see `scripts/excalidraw-compat/`.

MVP (slices 1–5): about 3–3.5 weeks. Slice 1 is independently useful: it is
the write half any editor, including a native one, needs.

## 7. Open questions for the owner

1. **Scope approval.** This adds a writer for `.excalidraw(.md)` files.
   `docs/excalidraw-reader.md` says reading never rewrites a drawing; editing
   would be explicit and gated like note source editing. Approve as an
   extension of #354?
2. **Linked text in `parsed` drawings.** Show those elements in raw `[[…]]`
   form in the browser so links survive edits (recommended)? The alternative
   is accepting that editing them flattens the link.
3. **Conflicts.** Keep the MVP at Reload / Keep mine / Save as copy, matching
   notes? Or approve an element-level three-way merge for drawings only?
4. **`draw.oklabs.uk`.** Retire it from Tessera entirely, or keep a "Copy
   scene" fallback?
5. **Bundle size.** Is about 10–23 MB of editor assets in the app acceptable,
   or should CJK fonts load lazily or be omitted?

## 8. Not verified here

- Native GUI behaviour: nothing was built or run in GPUI for this research.
- Safari and Firefox loading the loopback page. Only headless Chromium was
  probed.
- The Obsidian plugin reading a Tessera-written file. The format rules above
  come from source reading; slice 2 is the positive control.
- Whether the plugin migrates `data:` images in `files` into vault
  attachments.
- macOS wry/GPUI event-loop coexistence (option F).
