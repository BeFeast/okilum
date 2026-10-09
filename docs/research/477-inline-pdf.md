# Research: inline PDF viewing (#477)

> **Historical document.** Written before Tessera was renamed Okilum (2026-10-09). Names, paths and links are kept as they were then.

Status: research only. No product code changes. This document informs a go/no-go
decision and a slice plan for reading PDFs inside Tessera. Today a PDF opens as a
file card (on macOS, a Quick Look first-page thumbnail), and reading it means
pressing Space for Quick Look or opening another app.

The owner's framing: inline reading would be nice, the priority is low, and the
feature should be dropped if it is heavy.

## TL;DR

- **It is not heavy if Tessera uses hayro.** hayro is a pure-Rust PDF rasterizer
  (MIT/Apache-2.0, no `unsafe`, no native library). Typst uses it for PDF images.
  On three 300-page documents it matched PDFium on speed and memory. Its renders
  were as close to poppler's as PDFium's and MuPDF's were, at the pixel level and
  by eye. It ships as a normal crate on all three platforms, with no extra
  binary to sign, notarize or package.
- **PDFium is the fallback**, not the first choice. It is equally fast, more
  mature, and has a complete text API (character boxes, native search). The
  cost is a prebuilt native library of about 7.3–7.9 MB per platform, to be
  bundled, signed and listed in the notices by hand on macOS, Arch and Windows.
- **MuPDF is a no-go on license grounds.** It is AGPL-3.0. Tessera is MIT, and
  `about.toml` does not accept AGPL. A commercial Artifex license is the only
  way in. MuPDF was also the slowest to the first page on the math-heavy
  document, and its default cache used the most memory.
- **PDFKit (macOS) is the best single-platform engine**, at no binary cost. But
  it needs an Objective-C bridge, and it would leave Linux and Windows on a
  different engine. Windows' own `Windows.Data.Pdf` cannot extract text. Three
  code paths for a low-priority feature is the heavy option this doc
  recommends against.
- **Pure-Rust alternatives other than hayro are not viable.** `pdf` (pdf-rs) is
  a parser with no maintained published rasterizer. The `pdf-render` 1.0 crate
  on crates.io is an unrelated four-month-old project with about 1.9k
  downloads.
- **Recommendation: go, scoped.**
  - Build a read-only, page-virtualized hayro viewer that replaces the PDF file
    card. Then add in-PDF find. Text selection and links come later, and only
    if the first slices are used.
  - Estimate: about 1 week to the first useful slice (scroll and read).
  - About 2 weeks with find.
  - About 4–5 weeks (20–26 days) for the whole plan.
  - Quick Look and **Open with default app** stay available throughout.

## 1. Where PDFs render today

| What | Where |
|---|---|
| A selected attachment becomes a `FilePreview` (details line, image flag, optional thumbnail) | `crates/tessera-shell/src/reader_files.rs:119` (`FilePreview`), `:270` (`Reader::preview_file`) |
| The file card itself: header, icon, `PDF · N bytes · date`, Quick Look thumbnail or icon | `crates/tessera-shell/src/reader_files.rs:320` (`render_file_preview`) |
| Quick Look action (`qlmanage -p`, macOS only) and the file context menu | `crates/tessera-shell/src/reader_files.rs:39`, `:93` (`menu`) |
| Context menu on an attachment link inside a note (`tessera://attachment/…`) | `crates/tessera-shell/src/reader_files.rs:209` (`file_link_menu`) |
| First-page thumbnail entity: selection-owned, cancelled when the view drops, 10 s timeout, 2048 px cap, revision check against file changes | `crates/tessera-shell/src/reader_thumbnail.rs:5`, `:33` (`eligible`), `:50` (`Thumbnail`) |
| Native thumbnail bridge (`QLThumbnailGenerator`, 1024×1024 @2x) and its link flags | `crates/tessera-shell/src/thumbnail/bridge.m`, `crates/tessera-shell/build.rs:114` |
| Responsive block-image element (fits the column width, keeps intrinsic pixels out of layout) | `crates/tessera-shell/src/reader_image.rs` |
| `![[file.pdf]]` is turned into a plain `[[file.pdf]]` link; only image extensions become images | `crates/tessera-core/src/render.rs:57-65`; non-Markdown embeds are skipped by embed expansion at `render.rs:717` |
| PDF is classified as an attachment extension | `crates/tessera-core/src/link_rewrite/mod.rs:427`, `EntryKind::Attachment` in `crates/tessera-core/src/vault.rs` |
| Product contract: PDF is in the rendering acceptance corpus; the owner's vault has 24 notes with PDF links ("plain link") | `docs/PRD.md` §5 |

So the integration point is narrow. `.pdf` gets its own branch in
`render_file_preview`, next to the Excalidraw and thumbnail branches. That
branch holds a selection-owned viewer entity, built like `Thumbnail`: dropping
the entity cancels its work. Embeds (`![[x.pdf]]`, `#page=N`) are a separate,
later slice.

On Linux and Windows there is no thumbnail today. A PDF shows only the icon and
the details line, so an inline viewer is a bigger gain there than on macOS.

## 2. Candidates

| Engine | Kind | License | Maturity (2026-10) | Text API | Ships as |
|---|---|---|---|---|---|
| **hayro** 0.8.0 | Pure Rust (`#![forbid(unsafe_code)]`), CPU rasterizer on `vello_cpu` | MIT OR Apache-2.0; embedded standard-14 fonts are PDFium's Foxit fonts (BSD-3); CMYK ICC profile CC0 | 9 releases since 2025-07, breaking about every 1–2 months (0.5 Jan, 0.6 Apr, 0.7 May/Jun, 0.8 Oct). Single main author (LaurenzV). ~3M downloads, 43 reverse deps incl. `typst-render`/`typst-svg`. Regression suite >1000 PDFs scraped from pdf.js and PDFBox | **None.** Glyph runs carry Unicode (`Glyph::as_unicode`) and transforms; a custom `Device` builds the text layer | Crate; 15 new crates over Tessera's current `Cargo.lock` |
| **PDFium** via `pdfium-render` 0.9.4 | C++ (Chrome's engine) behind a Rust wrapper, dynamically loaded | PDFium BSD-3 plus bundled third-party code (FreeType FTL, ICU, lcms, libjpeg-turbo IJG, OpenJPEG, libpng, zlib, abseil, AGG 2.3, HarfBuzz, simdutf, fast_float, dragonbox, llvm-libc); wrapper MIT OR Apache-2.0; binaries from `bblanchon/pdfium-binaries` (MIT packaging, build 157.0.8086) | Engine: very mature. Wrapper: 89 releases since 2022; 0.9.4 fixed two double-frees | **Complete**: per-char boxes, `FPDFText_Find*` search with segment rects, links, form fields | Prebuilt `.so`/`.dylib`/`.dll` that we bundle; 5 new crates |
| **MuPDF** via `mupdf` 0.8.0 | C, compiled from source by `mupdf-sys` | **AGPL-3.0** (or commercial from Artifex) | Engine mature; wrapper 23 releases | Complete (structured text, search quads) | Static, ~1m35s cold C build on 4 cores |
| **PDFKit** (macOS) | System framework | Apple system framework, nothing to ship | Mature | Complete (`PDFPage.string`, `PDFSelection`, `findString:`) | 0 bytes; needs an ObjC bridge like `thumbnail/bridge.m`. GPUI cannot host a `PDFView` (an `NSView`), so pages would still be drawn into a bitmap |
| `Windows.Data.Pdf` | WinRT | System | Mature | **No text extraction or search** | Would still need a second engine for find |
| poppler | C++ | GPL-2.0/3.0 | Mature | Complete | Ruled out by license as MuPDF is; used here only as the **reference renderer** for fidelity |
| `pdf` (pdf-rs) 0.10 | Pure Rust parser | MIT | Parser; no maintained rasterizer on crates.io | Partial | Not benchmarked: no renderer to measure |

`gpui-pdf` 0.6.1 (MIT, 2026-09, 41 downloads, one author) is a GPUI viewer built
on hayro 0.7. It has page virtualization with atlas eviction, a hayro text-layer
`Device`, search, outline and forms. It shows that the approach works on the
same `gpui-pre` family Tessera uses. It is **not** a dependency candidate:
- It is too young.
- It is on hayro 0.7.
- Its chrome (header controls, page counter, scroll-to-top button) does not
  follow Tessera's UI rules.
- Its `gpui-pre` pin must move in lockstep with ours.

Per `AGENTS.md`, borrow its design notes, not its code.

## 3. Benchmark

### 3.1 Method

- **Scratch crate** outside the repo (`/tmp/.../pdfbench`): one binary per
  engine plus an empty baseline binary. Release profile, `strip = true`,
  thin LTO.
- **Measurements:** wall-clock times from `Instant`; RSS from
  `/proc/self/status` (`VmRSS`, `VmHWM`).
- **Process model:** each measurement is a fresh process, so the cold start
  includes library load.
- **Render target:** 1600 px wide, about a 800 pt column at 2× scale, as RGBA
  with a white background. This is the size the viewer would actually upload.
- **Host:** Linux x86_64 cloud container, 4 vCPU, 15 GB RAM. All engines ran
  in the same session, twice. The tables show the second run, made on an
  otherwise idle machine; the first run agreed within noise.
- **Not portable:** these are **not** numbers for the owner's Mac (see §3.6).
- **Engine versions:**
  - hayro 0.8.0;
  - pdfium-render 0.9.4 on PDFium 157.0.8086;
  - mupdf 0.8.0 with the `base14-fonts` feature only.

Corpus (all exactly 300 pages):

| File | Source | Size | Character |
|---|---|---|---|
| `mml-300` | First 300 pages of *Mathematics for Machine Learning* (pdfTeX) | 16.1 MB | Dense math, Type 1 fonts, vector figures; page 1 is a 2214×3166 CMYK JPEG cover |
| `progit-300` | First 300 pages of *Pro Git* 2nd ed. (Asciidoctor PDF / Prawn) | 11.9 MB | Prose, code blocks, ~295 raster screenshots |
| `scan-300` | Synthetic scanned book: 300 grayscale 200-dpi JPEG pages, no text layer | 132 MB | Worst case for image decode and file size |

Pages were trimmed with PDFium (`FPDF_ImportPages`).

**Positive controls.** Each one shows that a probe could detect the thing it
reports.
- **Rendering happened.** Every engine's render of each measured page has a
  non-white ("ink") pixel count close to the others. On the first pages the
  three agree within 0.1%: `mml` page 1 has about 3.52M ink pixels in every
  engine. On the text pages the spread is up to 8%, from anti-aliasing on thin
  glyphs. An engine that painted nothing would show 0.
- **Text extraction found the text.** Search hit counts agree across engines:
  539–548 for "matrix" in `mml`, and exactly 1109 for "branch" in `progit`.
  The scan's 0 characters is the expected absence: it has no text layer, and
  the same probe finds hundreds of thousands of characters in the other files.
- **The fidelity metric can tell pages apart.** Diffing a render of the wrong
  page against poppler gives mean 11.5 and 5.0% of pixels over 64. Correct
  pages score 0.7–7.4 and 0.02–1.5%.

### 3.2 Time to first page

Measured from process start to having the first page as an RGBA buffer, over
3 cold processes. "Jump" opens the file and renders page index 150 (page 151). GPU upload is not
included; see §3.5.

| Document | PDFium | MuPDF | hayro |
|---|---|---|---|
| `mml-300` first page (6.7 MB CMYK JPEG cover) | 488–540 ms | 1137–1345 ms | 499–650 ms |
| `mml-300` jump to page 151 | 42–43 ms | 49 ms | 57–74 ms |
| `progit-300` first page | 77–100 ms | 108–154 ms | 63–85 ms |
| `progit-300` jump to page 151 | 40–42 ms | 39–41 ms | 48–62 ms |
| `scan-300` first page | 59–83 ms | 74–105 ms | 163–183 ms (file read into memory); **76–102 ms memory-mapped** |
| Open + page count | 0.4–1.1 ms | 1.3–1.5 ms | 11–16 ms (text PDFs); 2.5–3.4 ms mapped / 104 ms read for `scan-300` |

On `mml` the first page is slow because of a large CMYK JPEG cover, and it is
slow in every engine. The time is spent decoding content, so no choice of
engine fixes it. The viewer must show a correctly sized empty slot straight
away and fill it when the bitmap lands.

### 3.3 Rendering all 300 pages and memory

The engine renders every page in order, single-threaded. Each bitmap is dropped
before the next page, as a virtualized view would do. The table gives
per-page times as mean / p95 / max, and peak process RSS (`VmHWM`) after the
whole sweep.

| Document | PDFium | MuPDF | hayro |
|---|---|---|---|
| `mml-300` | 22.7 / 30.2 / 582 ms · **94 MB** | 21.8 / 46.0 / 757 ms · **123 MB** | 20.8 / 28.7 / 407 ms · **77 MB** |
| `progit-300` | 40.8 / 95.9 / 183 ms · **69 MB** | 41.3 / 117.5 / 240 ms · **342 MB** | 32.9 / 67.5 / 116 ms · **66 MB** |
| `scan-300` | 55.8 / 69.0 / 92 ms · **162 MB** | 57.8 / 77.4 / 105 ms · **299 MB** | 58.1 / 75.8 / 87 ms · **165 MB**¹ |

¹ That run read the 132 MB file into memory. With a memory map, open RSS falls
from 131 MB to 24 MB, and the file pages are clean and reclaimable.

Notes:

- **Memory is dominated by bitmaps, not by the engine.** One 1600 px page is
  13–15 MB of RGBA on the CPU, and the same again in the GPU atlas. A viewer
  that keeps the visible pages ±1 resident holds about 4–6 pages, or 60–90 MB
  per side. A higher zoom needs tiling (slice 5), not bigger bitmaps.
- MuPDF's growth on `progit` is its default 256 MB resource store. It can be
  tuned, but it is still the worst default here.
- No engine leaked across 300 pages. RSS after the sweep stayed bounded.

### 3.4 Text extraction and search, all 300 pages

| Document | PDFium extract · native search | MuPDF extract · native search | hayro (custom glyph `Device`) |
|---|---|---|---|
| `mml-300` | 819 ms (645k chars) · 793 ms, 540 hits | 537 ms (631k) · 577 ms, 548 hits | 340 ms (577k chars), 540 hits |
| `progit-300` | 334 ms (494k) · 322 ms, 1109 hits | 214 ms (499k) · 251 ms, 1109 hits | 78 ms (498k), 1109 hits |
| `scan-300` | 0 chars (no text layer) | 0 chars | 0 chars |

hayro is fastest here because the probe does less work. It is a plain
glyph-order dump: no word or line reconstruction, no reading order and no
character boxes. That explains the lower character count on `mml`, where
spaces are inferred differently.

A real text layer has to return a rectangle for every character and group
characters into words and lines. That is about 500–600 lines of Tessera code;
`gpui-pdf`'s `text.rs` is 574 lines. PDFium and MuPDF give character boxes and
search quads directly.

For find-in-PDF the hayro cost is still small. Search results stream per page
on a background task, and 300 pages take well under a second on every engine.

Scanned PDFs have no text, so find reports that the document has no searchable
text. OCR is out of scope.

### 3.5 GPUI integration probe

A second scratch crate, pinned to the exact `gpui-pre 0.3.3` in Tessera's
`Cargo.lock`, does the following:

1. Rasterizes `mml-300` page 61 with hayro at 1600 px.
2. Converts it from RGBA to BGRA. GPUI's `RenderImage` is BGRA; with an opaque
   white background, premultiplied and straight alpha are the same.
3. Wraps it in `RenderImage::new(vec![Frame::new(..)])` and paints it with
   `img(ImageSource::Render(..))` in a headless `TestAppContext` window.
4. Removes it with `Window::drop_image`.

Result: the test passed in 3 of 3 runs.

| Check | Outcome |
|---|---|
| Page size and layout | 1600×2262 device px, laid out at 800×1131 logical px |
| `has_image_atlas_entry` before paint (negative control) | false |
| `has_image_atlas_entry` after paint | true |
| `has_image_atlas_entry` after `drop_image` | false |
| Time, probe crate optimized | 30–33 ms raster plus 19 ms RGBA→BGRA conversion |

The conversion pass is not free. It belongs on the background executor next to
the raster, or can be removed by rendering into a BGRA target directly. The
probe's first, unoptimized build measured 215 ms for the same conversion.

Implication: GPUI needs no patch. Its public `RenderImage` and `drop_image` API
is enough, and the gpui-core-stays-unpatched rule holds.

Eviction must call `drop_image` explicitly. Dropping the last
`Arc<RenderImage>` frees the CPU copy, but not the atlas texture. No code in
`crates/` calls `drop_image` today. The Quick Look thumbnail holds one image
per selection, so any atlas residue there is small; that is worth a separate
check. A 300-page scroll would leak atlas memory without explicit eviction.

### 3.6 What these numbers do not show

- **Native GUI behaviour and macOS timings.** The owner's Mac is the acceptance
  machine (cf. `docs/single-file-viewer.md`). Treat the ratios between engines
  as the evidence; the absolute milliseconds will not carry over. PDFKit was
  not measured: there is no macOS host in this session.
- **Parallel rendering.** PDFium is effectively single-threaded: the
  `thread_safe` feature wraps every call in one global mutex. hayro's `Pdf` can
  be shared; its `RenderCache` is `Rc`-based, so the viewer keeps one per
  worker. Rendering two adjacent pages in parallel is possible with hayro and
  not with PDFium. This was not measured.
- **Coverage of exotic PDFs.** hayro's README lists blending/isolation,
  knockout groups and colour-key masking as gaps. It does support
  password-protected files (RC4, AES-128, AES-256). The fidelity check covered
  7 pages of two real-world producers (pdfTeX, Prawn). The owner's 24 linked
  PDFs are the real acceptance corpus (slice 1 gate).

### 3.7 Fidelity versus poppler

Mean absolute grey difference against `pdftoppm` at the same width, and the
share of pixels that differ by more than 64 of 255. Lower is closer.

| Page | PDFium | MuPDF | hayro |
|---|---|---|---|
| `mml` 1 (cover) | 6.57 · 0.51% | 7.37 · 0.59% | 6.81 · 0.54% |
| `mml` 61 (display math) | 2.91 · 1.52% | 2.68 · 1.48% | 2.39 · 1.40% |
| `mml` 151 | 1.96 · 1.05% | 2.12 · 1.08% | 1.71 · 0.99% |
| `mml` 251 | 2.83 · 1.47% | 2.79 · 1.32% | 2.33 · 1.37% |
| `progit` 1 (cover art) | 0.74 · 0.05% | 0.69 · 0.02% | 1.59 · 0.34% |
| `progit` 101 | 1.43 · 0.74% | 1.16 · 0.58% | 1.25 · 0.70% |
| `progit` 201 | 1.75 · 0.80% | 1.61 · 0.75% | 1.72 · 0.79% |
| Control: PDFium page 61 against poppler page 151 | 11.52 · 5.00% | | |

The remaining differences are anti-aliasing. Side-by-side crops of hayro and
poppler on `mml` page 61 (sums, tildes, coloured indices) and on the `progit`
cover could not be told apart by eye.

## 4. Binary size and build cost

Stripped release binaries. "Δ" is measured against an empty Rust binary of
0.34 MB.

| Engine | In-binary Δ | Extra file shipped | gzip (binary + extra) |
|---|---|---|---|
| hayro (default: embedded fonts + CMaps) | +6.56 MB | — | 2.94 MB |
| hayro without `embed-fonts`/`embed-cmaps` | +5.81 MB | — | 2.35 MB |
| PDFium | +1.24 MB (wrapper) | `libpdfium.so` 7.94 MB (Linux x64), `libpdfium.dylib` 7.34 MB (macOS arm64; the universal archive is about 2× as large), `pdfium.dll` 7.49 MB (Windows x64) | 0.48 + 3.4–3.7 MB |
| MuPDF (base-14 fonts only, no CJK) | +5.58 MB | — | 3.25 MB |
| PDFKit | 0 | — | 0 |

The hayro Δ is an upper bound for Tessera. The bench binary pays for `image`,
PNG/JPEG decoding and `kurbo`, which Tessera already links.

Only 15 crate names are new over the current lockfile: the `hayro-*` family,
`vello_cpu`, `vello_common`, `peniko`, `color`, `fearless_simd` (two versions),
`pic-scale` and `guillotiere`. It would also add a second copy of
`skrifa`/`read-fonts` (0.46/0.43, next to Tessera's 0.40/0.37 and 0.44/0.41).
Every one of these crates is MIT, Apache-2.0, BSD-3-Clause, Zlib or
Unicode-3.0. The embedded fonts and CMaps should stay on. Without them,
PDFs that rely on standard fonts that are not embedded, or on CJK CMaps,
render wrongly.

**Distribution cost of PDFium**, beyond bytes:
- **macOS:** the library goes into the `.app`, is signed and notarized with the
  bundle, and is loaded from the bundle path.
- **Windows:** one more DLL in the Velopack package.
- **Arch:** one more file in the `tessera` package (`/usr/lib/tessera/`).
- **Licensing:** a hand-written notice for PDFium and the 16 third-party
  licence files bundled with it. Like Sparkle, it is not a crate, so
  `cargo-about` does not see it.
- **Upkeep:** a version bump flow outside Cargo.

hayro has none of these. Its only manual item is a notice line for the Foxit
fonts (BSD-3).

## 5. Licensing summary

- **hayro**: MIT OR Apache-2.0. Its dependencies are MIT, Apache-2.0 or
  BSD-3-Clause; all are in `about.toml`'s accepted list. The embedded Foxit
  fonts are BSD-3 (from PDFium). The CMYK profile is CC0.
  Action: add the Foxit notice to `docs/third-party-notices.md` and `licenses/`.
- **PDFium**: BSD-3 plus the bundled set listed in §2. All are permissive.
  FreeType is dual FTL/GPL-2; we would take FTL, which requires a credit in
  the documentation. The IJG licence requires the "based in part on the work
  of the Independent JPEG Group" acknowledgement. Acceptable, but every item
  is a manual notice.
- **MuPDF**: AGPL-3.0. Linking it would put Tessera's distribution under AGPL
  terms. That conflicts with Tessera's MIT licence and with `about.toml`.
  **Excluded** unless a commercial licence is bought.
- **poppler**: GPL. Excluded for the same reason.
- **PDFKit / Windows.Data.Pdf**: system frameworks. Nothing to ship or notice.

## 6. Recommendation

**Go, with hayro, in thin slices. Each slice must be useful on its own, and the
work stops after any slice where usage does not justify the next.**

Why hayro over PDFium:
- Equal speed and memory on these documents.
- Equal fidelity on these pages.
- One code path on all three platforms.
- No native binary to package, sign or notice.
- No `unsafe` at the FFI boundary.
- An API that fits a background-rendering design.

The price is hayro's youth: 0.x, breaking releases every one or two months,
and one main author. The plan contains that risk:

- **Behind a seam.** The viewer talks to a small Tessera-owned trait:
  `open`, `page_count`, `page_size`, `render(page, px_width) -> BGRA`,
  `text_layer(page)`. Only one module knows about hayro.
- **Pinned and gated.** hayro stays at an exact version in `Cargo.lock`, and an
  upgrade is a reviewed change. The owner's 24 linked PDFs form a fidelity
  corpus that every upgrade must pass.
- **Fallback, not a lowered bar.** If hayro fails an acceptance gate, the
  component moves to PDFium behind the same trait, at the packaging cost in
  §4. This follows the "quality gates do not move" rule.

Not recommended:
- **PDFKit on macOS plus another engine elsewhere.** Two or three
  implementations of render, text and search, plus ObjC bridging, for a
  low-priority feature.
- **MuPDF**, for licence reasons.

## 7. Slice plan

Estimates are focused engineering days for one developer, including tests and
review. They assume the existing `Thumbnail` pattern: a selection-owned entity,
background work cancelled on drop, and a revision check that refuses stale
results.

| # | Slice | Scope | Estimate |
|---|---|---|---|
| 1 | **Read a PDF inline** | New `reader_pdf.rs` behind the engine trait (hayro, memory-mapped file). `.pdf` branch in `render_file_preview`. All page slots sized up front from page dimensions so the scrollbar is correct immediately. Visible ±1 pages rasterized on the background executor at column width × scale factor, LRU budget (for example 6 pages), eviction with `window.drop_image`. File-changed revision check and re-open. Inline "can't display this PDF" state, with Quick Look / **Open with default app** kept as escape hatches. Password-protected files show an inline locked state; no modal. Tests: headless GPUI layout and atlas eviction (positive and negative control, as in §3.5), a fixture PDF, the large-document memory ceiling | 5–6 d |
| 2 | **Find in PDF** | Reuse the existing find bar (`find_open`, the `FindInNote` action) and its ⌘F/Ctrl+F binding. A page-ordered background scan through a hayro text device with per-glyph boxes. Hits highlighted as overlays in normalized page coordinates; next/previous scrolls to the hit. Scanned files say plainly that they have no searchable text | 4–5 d |
| 3 | **Select and copy text** | Word and line grouping in the text layer, a drag-selection overlay, copy to clipboard, double-click selects a word | 4–5 d |
| 4 | **Links and outline** | Internal link → jump to page; external link → the existing link handling. The document outline as the reader's heading list, if it fits the current sidebar model | 2–3 d |
| 5 | **Zoom and tiles** | Fit-width, plus zoom steps with ⌘/Ctrl +/−. Above 2× zoom, render only visible tiles, so the bitmap budget does not grow with zoom | 3–4 d |
| 6 | **PDF in notes** | `![[file.pdf]]` and `![[file.pdf#page=N]]` as an inline page block, using the same element, sized by `reader_image`'s rules | 2–3 d |

- **First useful slice:** about 1 week (slice 1).
- **With find:** about 2 weeks (slices 1–2), which covers most of the reason
  to read a PDF in Tessera rather than in Quick Look.
- **Whole plan:** about 4–5 weeks (20–26 days).
- **Not in this plan:**
  - PDF text in vault search (Tantivy). It is derived data and would have to
    follow the rebuildable-index rules; it gets its own issue.
  - Annotations or form filling. The reader is read-only.
  - OCR.

### Acceptance gates for slice 1

Measured on the owner's Mac, with baseline and comparison in the same session:

- **Every one of the owner's 24 linked PDFs:**
  - opens inline, and pages show no visible defect next to Preview.app;
  - shows a correctly sized first page slot before the bitmap arrives;
  - shows a text PDF's first page in under 300 ms.
- **Fast scrolling through a 300-page PDF:**
  - no frame is dropped because of rendering; all raster work stays off the
    UI thread;
  - resident memory stays within the page budget, about 150 MB above the
    idle reader.
- **Leaving the PDF:**
  - cancels outstanding renders;
  - releases the atlas, proven with `has_image_atlas_entry` in a headless test
    with both controls.

## Appendix: reproducing

The scratch crates and scripts were throwaway evidence and are not committed.
These steps are enough to rebuild them:

1. `cargo new` a workspace with one binary per engine:
   - `pdfium-render = { version = "0.9.4", default-features = false, features = ["thread_safe", "pdfium_latest"] }`;
   - `hayro = "0.8.0"`;
   - `mupdf = { version = "0.8.0", default-features = false, features = ["base14-fonts"] }`.
2. Get PDFium from
   `https://github.com/bblanchon/pdfium-binaries/releases/latest/download/pdfium-linux-x64.tgz`.
3. Download the corpus from:
   - `https://mml-book.github.io/book/mml-book.pdf`;
   - `https://github.com/progit/progit2/releases/download/2.1.449/progit.pdf`.

   Trim each to 300 pages with `FPDF_ImportPages`. Generate `scan-300` with
   Pillow: 300 pages of 1700×2200 grayscale, JPEG quality 70, at 200 dpi.
4. Run each binary as `<engine> <file> 1600 <needle> first|jump|full`.
   - Each run prints open, first-page, per-page distribution, RSS/HWM,
     text-extraction and search figures.
   - `TESSERA_DUMP=<file.ppm>` writes the measured page as a PPM for the
     fidelity diff against `pdftoppm -scale-to-x 1600`.
   - `TESSERA_JUMP=<n>` picks the jump page.
   - `TESSERA_MMAP=1` makes hayro use a memory map.
5. For the GPUI probe:
   - Make a crate with
     `gpui = { package = "gpui-pre", version = "=0.3.3", features = ["test-support"] }`,
     `hayro = "0.8.0"` and `image = { version = "0.25", default-features = false }`.
   - Copy Tessera's `Cargo.lock` into it so the same GPUI is resolved.
   - Write one `#[gpui::test]` that paints the page with `img(ImageSource::Render(..))`
     and checks `Window::has_image_atlas_entry` before paint, after paint, and
     after `drop_image`.
   - Add `use ::core::prelude::v1::test;` in the test module, as Tessera's own
     tests do. Without it the `#[test]` expansion recurses.
