# Research: structured log viewer based on `hl` (#602)

Status: research only. No product code changes. This document informs a go/no-go
decision and a slice plan for the log viewer requested in #602.

Request: a log viewer in Tessera based on [`pamburus/hl`](https://github.com/pamburus/hl)
(Rust, MIT). hl is a fast viewer for JSON and logfmt structured logs. It
auto-detects the format, reads gz/xz/zstd/bz2, filters by level, field and time
range, has a query language, sorts several files chronologically, and has a follow
mode and themes. The viewer should handle:

- `.log` and `.jsonl` files inside and outside a vault (the single-file quick
  viewer, #560),
- Tessera's own diagnostic log (JSON Lines),
- service logs.

## TL;DR

- **hl is a CLI that happens to compile as a library. It is not a usable
  library.**
  - It is not published on crates.io (the `hl` name belongs to an unrelated
    crate), so it could only be used as a git dependency.
  - Its `build.rs` makes network requests at build time.
  - The parsed record type, `Record`, is in a private module, so outside code
    cannot name it (proven with a compile probe, below).
  - The only working in-process entry point is `App::run`, which writes
    ANSI-colored text.
  - Embedding it adds 99 crates that Tessera does not have today.
- **Bundling the hl binary** works and is fast. But it ships a third executable
  on every platform, and Tessera gets back terminal text instead of records.
  That rules out field chips, click-to-filter and a detail pane.
- **A small parser of our own in `tessera-core`**, built on crates Tessera
  already locks (`serde_json`, `memchr`, `time`), indexed the 100 MB test file as
  fast as hl parses it. It added about 0.16 MB to a stripped binary.
- **Recommendation: option (c).**
  - Write our own minimal JSON-lines and logfmt indexer in `tessera-core`, and a
    GPUI log view in `tessera-shell`.
  - Borrow hl's ideas explicitly, not its code: field-name aliases, level
    normalisation, query syntax, parallel chunked scanning, and a timestamp index
    for merging.
  - Estimate: about 5–6 weeks across 9 slices. The first useful slice (open a
    `.jsonl`/`.log` file and see colored, virtualized rows) takes about 1.5
    weeks.

## 1. Can hl be used as a library?

Evidence comes from cloning `pamburus/hl` at `c1ac299f` (2026-10-02, version
0.36.3), building it, and compiling two probe crates against it with a `path`
dependency. All of this was done outside the repo, in a scratch directory.

### 1.1 Workspace layout and crate names

The root package `hl` is both a binary (`src/main.rs`) and a library
(`src/lib.rs`). These are the workspace members:

| Crate | Version | Purpose | On crates.io? |
|---|---|---|---|
| `hl` | 0.36.3 | everything: parser, model, query, formatter, index, follow, app | **No** (the `hl` name is a different 2020 crate, `twe4ked/hl` 0.4.0) |
| `serde-logfmt` | 0.1.2 | logfmt deserializer | No (`crate serde-logfmt does not exist`) |
| `encstr` | 0.1.2 | encoded JSON/raw strings | No |
| `heapopt`, `enumset-ext`, `enumset-serde`, `lifecycle`, `mline`, `pager`, `styled-help`, `wildcard` | 0.1–0.2 | helpers | No |

None of the crates has a `publish` setting, but none is on crates.io either. The
root package also depends on a **git fork of `clap_mangen`**
(`pamburus/rust-clap@20f0ffd7`). crates.io refuses git dependencies, so it cannot
be published as it stands. The README only documents
`cargo install --git …`.

### 1.2 Public API surface

`src/lib.rs` re-exports `App`, `Options`, `SegmentProcessor`, `Parser`,
`ParserSettings`, `Filter`, `FieldFilterSet`, `Level`, `Query`,
`RecordFormatter`, `Settings` and `Theme`. The data model lives in `mod model`,
which is **private**:

```rust
// hl src/model.rs:318 — the parsed record
pub struct Record<'a> {
    pub ts: Option<Timestamp<'a>>,
    pub message: Option<RawValue<'a>>,
    pub level: Option<Level>,
    pub logger: Option<&'a str>,
    pub caller: Caller<'a>,
    pub(crate) fields: RecordFields<'a>,
    ...
}
```

`Record` is never re-exported. `RawRecord`, which `Parser::parse` takes as input,
is not re-exported either.

**Probe 1: get structured records out of hl.** I compiled a scratch crate
against hl with three variants:

1. Implement `hl::app::RecordObserver` for a struct. This fails with
   `error[E0603]: module 'model' is private`. The trait's method signature
   mentions `Record`, which outside code cannot name.
2. Pass a closure `|r: &_, loc| …` through the blanket
   `impl<T: FnMut(&Record, Range<usize>)> RecordObserver for T`.
   - Type inference does resolve to `hl::model::Record`.
   - Accessing a field such as `r.level` in the closure body fails with
     `error[E0282]: type annotations needed`.
3. A generic helper `fn peek<R>(r: &R)` compiles, but it cannot read any
   field.

Outside code therefore **cannot read parsed records** without patching hl (make
`model` public or re-export `Record`/`RawRecord`). That patch would be a fork.

**Probe 2: run hl in-process and capture its text output.**

- Building `hl::Options` takes 27 public fields: theme, time format, filter,
  field options, formatting settings, time zone, input info, ASCII mode and so
  on. The caller also needs a direct dependency on `chrono-tz` just to name a
  time zone.
- With level ≥ error and `status>=500`, on the 100 MB file, `App::run` wrote
  4,054 lines (988 KB) into a `Vec<u8>`. A Python reference count gave the same
  4,054, so the probe really filters.
- Each line is formatted, ANSI-colored text. For example:
  `2026-10-01 00:00:00.240 \e[0;2m[\e[0;1mERR\e[0;2m] \e[0mvault.scan: … request-id=97c0… status=\e[0;32m500 …`.
- Byte offsets of records, the fields themselves, and the original key spelling
  are all lost. hl displays `request_id` as `request-id` and `elapsed_ms` as
  `elapsed-ms`.

### 1.3 Stability

- hl is 0.x. It shipped 7 minor (breaking-allowed) releases between v0.30.0
  (2024-12-21) and v0.36.0 (2026-02-28).
- There have been 342 commits since 2026-01-01, 96 of them touching
  `src/app.rs`.
- The repo makes no library-stability promise. A recent commit is marked
  breaking (`feat(pager)!: …`).
- `app.rs` itself says `// TODO: merge Options to Settings and replace Options
  with Settings.`, which means the main entry type is expected to change.

### 1.4 Dependency weight

The probe binary's dependency set has **338 packages**. Comparing it with
Tessera's `Cargo.lock`:

- **99 crate names are new to Tessera.** Examples: `clap_complete`,
  `clap_mangen` (git), `config`, `ron`, `json5`, `rust-ini`, `yaml-peg`,
  `yaml-rust2`, `capnp`, `ciborium`, `pest*`, `logos*`, `chrono-tz`,
  `chrono-english`, `jiff`, `liblzma`/`liblzma-sys` (C, statically linked),
  `owo-colors`, `env_logger`, `pager`, `signal-hook`, `snap`, `deko`. The
  build-time `ureq`/`rustls-platform-verifier`/`openssl`/`native-tls` stack
  also appears in the lock.
- **59 more resolve to versions Tessera does not have.** Notable: `notify` 8.2
  where Tessera has 7.0, `zstd-sys` 2.1.0 where Tessera has 2.0.16 (a `links`
  crate, so the build must unify on one version), `itertools` 0.15, `dirs` 7,
  `syn` 3, and a `toml` 1.x bump.

`build.rs` is not hermetic:

- It runs a Cap'n Proto generation step.
- It reads git build info.
- `update_schema_directives` **fetches remote URLs with `ureq`** to hash schemas
  and may rewrite TOML files. A failed fetch is downgraded to a warning, but a
  release build in Tessera's pinned container would still try to reach the
  network.

### 1.5 MSRV and toolchain

- hl: `rust-version = "1.86.0"`, `edition = "2024"`, and its
  `rust-toolchain.toml` pins `1.95`.
- Tessera's verified builds use Rust **1.96.1** (`docs/building.md:20`). There is
  no root `rust-toolchain` file, and the crates use `edition = "2021"`.
- The MSRV is compatible. The edition does not matter across crates. This is not
  a blocker.

### 1.6 Measurements (hl CLI, 100 MB synthetic input)

Machine and method:

- VM: Intel Xeon @ 2.10 GHz, 4 vCPU, 15 GiB RAM, Linux 6.18.
- hl 0.36.3 built with `--release --locked` (LTO, 1 CGU); the build took 2 min 57 s.
- Each run's stdout goes to `/dev/null`. The median of 3 runs is reported, and
  RSS is `ru_maxrss` from `wait4`.
- The input files were just written, so the page cache was warm for every run.
- All numbers come from one machine in one session (AGENTS.md "Compare on one
  machine").

Input data:

- `app.jsonl`: 104,857,627 bytes, 480,974 records, about 218 B/record. Fields:
  `ts`, `level` (30% debug / 55% info / 10% warn / 5% error), `logger`, `msg`,
  `request_id`, `duration_ms`, `status`, `path`, and a nested `user` object.
- `app.logfmt`: the same content as logfmt, 566,761 records.
- `app.jsonl.gz`: `gzip -1`, 20.5 MB.

| Run | Wall | CPU | Max RSS |
|---|---:|---:|---:|
| `wc -l` baseline | 0.03 s | 0.03 s | 9 MiB |
| hl format all, no color | 0.30 s | 1.08 s | 17 MiB |
| hl format all, `-c` color | 0.38 s | 1.26 s | 22 MiB |
| hl format all, `-C 1` (1 thread) | 1.04 s | 1.09 s | 12 MiB |
| hl `-l e` (level ≥ error) | 0.18 s | 0.58 s | 13 MiB |
| hl `-q 'status>=500 and duration_ms>800'` | 0.17 s | 0.59 s | 13 MiB |
| hl `--since/--until` 2-minute window | 0.20 s | 0.71 s | 16 MiB |
| hl format all, logfmt input | 0.31 s | 1.12 s | 18 MiB |
| hl format all, `.gz` input | 0.51 s | 1.42 s | 12 MiB |
| hl `-r -l e` (raw passthrough + filter) | 0.15 s | 0.51 s | 13 MiB |
| hl `-s` sort, cold (builds index, 80 KB cache) | 0.50 s | 1.79 s | 18 MiB |
| hl `-s` sort, warm index | 0.33 s | 1.21 s | 16 MiB |
| hl `-s -l e`, warm index | 0.16 s | 0.58 s | 15 MiB |

Each filter was checked against a known answer, so a filter that matched nothing
would have shown up:

- `-l e` printed 23,873 lines on JSON input, which matches
  `grep -c '"level":"error"'`.
- It printed 28,087 lines on logfmt input, which matches
  `grep -c ' level=error '`.
- Follow mode (`-F`) printed all 5 records appended at 300 ms intervals.

hl is fast because it streams. RSS stays at 12–22 MiB however large the file is,
and it uses 4 threads.

Two behaviours matter for Tessera:

- **Records without a level disappear under a level filter.** This is the
  shape of Tessera's own diagnostic log. `hl -l w` on it printed **0** lines,
  including a record that carries an `"error"` key. A viewer must not hide such
  records silently (AGENTS.md: "Ambiguity is surfaced, not guessed").
- Non-JSON lines are passed through as text, which is good. Key names are
  rewritten for display (`_` becomes `-`), so the display is not lossless.

### 1.7 Option (c) prototype: own indexer with already-locked crates

I wrote a ~120-line scratch prototype using only `serde_json` (with the
`raw_value` feature), `memchr` and `time`. It:

- splits the file on newlines into N chunks,
- parses each line as JSON (`HashMap<&str, &RawValue>`) or as logfmt,
- records `{byte offset, length, timestamp ns, level, message span}` (48 B per
  entry).

It does not format the output. Its closest hl counterpart is `hl -l e`, which
parses every record but formats only 5%.

| Run | Wall | CPU | Max RSS |
|---|---:|---:|---:|
| own, JSON, 1 thread | 0.63 s | 0.63 s | 146 MiB |
| own, JSON, 4 threads | 0.23 s | 0.61 s | 130 MiB |
| own, logfmt, 1 thread | 0.26 s | 0.26 s | 154 MiB |
| own, logfmt, 4 threads | 0.14 s | 0.27 s | 135 MiB |

The positive control matched: 23,873 and 28,087 errors, every record had a
timestamp, and the output stayed in order.

The RSS is high only because the prototype calls `fs::read` on the whole file.
The real design would memory-map or stream it. The index itself is
480,974 × 48 B ≈ 23 MB, and it can be shrunk (see §3.6). CPU per byte is the same
as hl's parse path.

### 1.8 Options compared

| | (a) Embed hl crates | (b) Bundle hl binary, render output | (c) Own minimal parser, hl ideas |
|---|---|---|---|
| Structured access (field chips, click-to-filter, detail pane, copy raw line) | Only with a fork patch to expose `model` | **No.** ANSI text only. JSON output mode doesn't exist; `-r` gives raw lines back | Yes, by design |
| Distribution | Git dependency on an unpublished crate plus a git fork of `clap_mangen` | Third executable per platform: macOS sign/notarise nested binary, Velopack payload, Arch PKGBUILD | None |
| Build hygiene | `build.rs` network fetch, Cap'n Proto codegen, C `liblzma` static | Separate pinned build of hl on each OS/arch | Uses crates already in `Cargo.lock` |
| New crates | 99 new plus 59 version splits (notify 7→8, zstd-sys) | 0 in Tessera, but hl's ~250-crate notice set must ship with it | 0 new for JSON/logfmt/gz/zstd/bz2; xz needs a decision (§4) |
| Binary size (stripped, Linux x86-64) | +~8.8 MB (probe 9.13 MB vs hello-world 0.34 MB, no LTO) | +9.4 MB (hl LTO build), 3.4 MB gzipped | +~0.16 MB (prototype 0.50 MB vs 0.34 MB) |
| API stability risk | High: 0.x, `Options` slated for rewrite | Medium: CLI flags are the contract | Ours |
| Feature breadth on day 1 | Highest | Highest | Grows slice by slice |
| Performance (100 MB) | hl numbers | hl numbers plus process and pipe overhead plus ANSI parse | Equal parse throughput (§1.7) |
| Fit with AGENTS.md | Fails "quality gates" on build hygiene and API access | Fails the core UX goals | Passes |

(a) fails two acceptance gates as it stands: no access to records, and a
non-hermetic build. AGENTS.md's rule is "replaced, or the component is written
from scratch", so (a) is out unless hl upstream exposes a library API. A
"library mode" proposal upstream could be filed as optional goodwill, but no
Tessera slice should depend on it. (b) technically works, but it turns a
structured-data feature into a terminal emulator.

## 2. How it fits Tessera's architecture

### 2.1 Responsibility split

`crates/tessera-core/src/lib.rs:1` describes core as "the half of Tessera that has
nothing to do with a window". The existing precedent is to keep pure parsers in
core and rendering in the shell:

- `tessera_core::excalidraw::Scene::parse` is rendered by
  `tessera-shell/src/reader_drawing.rs`.
- `source_classifier` and `callout` follow the same pattern.

Proposed new code:

**`tessera-core`: new module `log/`**

- `detect.rs`: sniff format from the first N KB. Outcomes: JSON lines, logfmt,
  mixed, or plain text. Also sniff compression from magic bytes.
- `record.rs`:
  - `LogEntry { offset: u64, len: u32, ts: Option<i64>, level: LevelTag, msg: Option<Span> }`
  - `LevelTag { Trace, Debug, Info, Warn, Error, Fatal, Unknown, Unparsed }`
- `index.rs`: parallel chunked indexing into a `LogIndex`, an append-only
  structure that supports follow mode.
- `fields.rs`: lazy field extraction for one record (detail pane and chips).
  It is lossless, keeps the original key spelling, and flattens nested objects
  as `a.b`.
- `query.rs`: a query subset compatible with hl's syntax (§3.3).
- `tail.rs`: an appended-bytes reader that handles truncation and rotation.

**`tessera-shell`: new `reader_log.rs`**

- A `LogView` entity holding `Arc<LogIndex>`, the filter state, a filtered
  row vector, the selection, and the detail pane.
- Rendering is described in §3.

**Not changed in v1**

- `tessera-cored` protocol, `EntryKind` (serde, part of the snapshot and
  protocol format), the warm snapshot, and search indexing.
- The viewer is read-only, consistent with the hard rule that the v0 reader is
  read-only.
- The index is in memory only and can always be rebuilt from the file. If a
  persistent index is ever added, it is a deletable cache, like hl's 80 KB
  `~/.cache/hl/…` index.

### 2.2 Entry points

**Single file outside a vault (#560 quick viewer)**

- `crates/tessera-shell/src/reader_open.rs:33` `OpenIntent::validate_inner`
  currently fails with `"Choose a local Markdown (.md) file or a directory"`
  (line 72), and then calls `read_to_string`. It must accept `.log`, `.jsonl`,
  `.ndjson`, `.logfmt` (and compressed variants), and must stop requiring UTF-8
  for those files.
- `crates/tessera-shell/src/reader_loading.rs:108`
  `prepare_first_with_last_document` has a single-file branch at about line 138.
  Add a sibling branch that builds a `LogIndex` on the background job and
  publishes a "log document" instead of calling `render::reader_document`.
- `quick_folder()` (about `reader_loading.rs:5320`) already lists siblings as
  `EntryKind::Attachment`, so neighbouring logs show up in the tree for free.

**A log inside a vault**

- Keep `EntryKind::Attachment` (`crates/tessera-core/src/vault.rs:159`).
- In `Reader::preview_file` (`crates/tessera-shell/src/reader_files.rs:253`),
  detect a log (extension plus sniff) and mount `LogView` instead of the
  metadata card from `FilePreview::load` (line 112) and `render_file_preview`
  (line 303).
- Drawings already use exactly this pattern, via
  `tessera_core::excalidraw::is_drawing` in `render_file_preview`.

**Tessera's own diagnostic log**

- The writer is `crates/tessera-shell/src/reader_diagnostics.rs`.
  - It writes JSON Lines (`serde_json::to_writer` plus `\n`, line 160) to
    `reader-diagnostic.log` (line 6).
  - It rotates at 4 MiB (line 7) to `reader-diagnostic.log.previous`.
  - Records look like `{"time": <unix seconds>, "launch", "phase", "elapsed_ms",
    "vault", "details" | "error" }`, with **no level field**.
- The location comes from `reader_history::state_directory()`
  (`reader_history.rs:15`).
- Proposal:
  - Add a command **Help → Open diagnostic log** that opens both files merged
    chronologically.
  - Separately, add `"level"` to diagnostic records (`warn` for scan warnings,
    `error` for `record_failure`). This is a small change in the writer, done as
    its own slice.

**OS file associations** (later slice)

- `scripts/arch/tessera.desktop` (`MimeType=text/markdown;`)
- `scripts/build-macos-ci.sh:59–77` `CFBundleDocumentTypes`
- the Windows ProgID in `crates/tessera-shell/src/markdown_handler.rs`

These should be "Open With" registrations only. Tessera should not become the
default app for `.log`.

### 2.3 List virtualization

- gpui `uniform_list` (gpui-pre 0.3.3 `src/elements/uniform_list.rs:22`) is
  already used for:
  - the sidebar tree (`crates/tessera-shell/src/main.rs:3745`),
  - quick-open results (`crates/tessera-shell/src/quick_open.rs:274`).

  It renders only the visible range from an item count. That fits fixed-height,
  single-line log rows over hundreds of thousands of entries, which is what the
  log view needs.
- gpui-kit's `v_virtual_list` (`vendor/gpui-component/crates/base`) needs a
  `Vec<Size>` covering every row. That is avoidable overhead for 500k rows.
- gpui-kit's `DataTable` (`TableDelegate` in
  `vendor/gpui-component/crates/component/src/table/delegate.rs`) could be
  used for a columns mode, but it is heavier than needed for v1.
- The Reader's `TextView` (`ListState` with one item per top-level block, as the
  comment at `main.rs:1759` explains) is **not** suitable. A log is one huge
  block.
- Choice: `uniform_list` of collapsed one-line rows, with the selected record
  expanded in a separate detail pane rather than inline. This keeps row height
  uniform. No new vendor patch is needed.

### 2.4 Theme colors

Level colors come from the existing semantic tokens. No literal colors:

- `crates/tessera-shell/src/brand.rs:146` `Palette` has `success`, `warning`,
  `danger` and `info` (lines 161–164), sourced from
  `assets/brand/brand-tokens.json`:
  - light: `#197446`, `#855700`, `#B52D48`, `#086C97`
  - dark: `#4ADE80`, `#FBBF24`, `#FB7185`, `#38BDF8`
- `ReaderPalette` (line 213) provides faint text and the code background.
- `callout_look` (`main.rs:654`) is the existing precedent for mapping severity
  to color and icon.

Proposed mapping:

| Level | Color | Badge |
|---|---|---|
| trace | `ReaderPalette.textFaint` | `TRC` |
| debug | `muted_foreground` | `DBG` |
| info | `Palette.info` | `INF` |
| warn | `Palette.warning` | `WRN` |
| error | `Palette.danger` | `ERR` |
| fatal | `Palette.danger`, bold, row tint | `FTL` |
| (no level) | `muted_foreground` | `---` |
| (unparsed line) | `ReaderPalette.textFaint`, monospace | `RAW` |

Light and dark mode follow automatically through `brand::apply_theme`
(`brand.rs:271`). hl's 23 themes are not imported. Tessera has one visual system.

### 2.5 Follow / tail

- `tessera_core::watch::VaultWatcher` (`crates/tessera-core/src/watch.rs`) uses
  notify 7 and a 300 ms `QUIET_WINDOW` (line 61). It filters out every non-`.md`
  path (lines 141 and 204–209), so it is not reusable as-is.
- `log/tail.rs` should own a single-file watcher. It should:
  - use notify 7, which is already a core dependency, on the parent directory
    so that rotation is visible;
  - detect truncation (size shrinks) and rotation (file identity changes; hl
    has `win_file_id.rs` for Windows, and on Unix this is inode/dev);
  - fall back to polling on roots where `is_network_root()` (watch.rs:65) is
    true.
- New bytes are indexed incrementally and appended to `LogIndex`. Because the
  index is append-only, the UI keeps its scroll position unless the view is
  "pinned to bottom".

## 3. UX proposal

### 3.1 Layout

```
┌ Tessera ─ app.jsonl ────────────────────────────────────────────── ● Follow ┐
│ [Level: ≥ INF ▾] [Time: last 15 min ▾] [ status>=500 and path~="/v1"     ⏎ ]│
│ chips: (logger = vault.scan ✕) (user.role = owner ✕)      480,974 → 1,203 │
├────────────────────────────────────────────────────────────────────────────┤
│ 00:00:00.240  ERR  vault.scan  scan complete  status=500 path=/v1/notes/… │
│ 00:00:00.244  WRN  http        retrying upload  status=503 duration_ms=812│
│▶00:00:00.251  ERR  reader      cache miss  status=500 request_id=97c0…    │
│ 00:00:00.260  ---  —           launch=3f1c phase=inventory_first_paint …  │
│ 00:00:00.262  RAW  not json at all                                         │
│ …                                                              ▓ scrollbar│
├─ Record 3 of 1,203 · line 118,402 · byte 25,812,330 ───────── [Copy raw] ─┤
│ ts           2026-10-01T00:00:00.251Z        [=] [≠] [≥] [≤]              │
│ level        error                           [=] [≠]                       │
│ logger       reader                          [=] [≠]                       │
│ msg          cache miss                                                    │
│ request_id   97c0482241e0de67                [=] [≠]                       │
│ status       500                             [=] [≠] [≥] [≤]              │
│ user.id      2210                            [=] …                          │
│ user.role    owner                           [=] …                          │
└────────────────────────────────────────────────────────────────────────────┘
 JSON lines · UTC ▾ · indexed 104.9 MB in 0.23 s · 2 unparsed lines
```

### 3.2 Rows

- Each row shows: time column, level badge, logger, message, then the first
  remaining fields as `key=value` in muted color, all on one line with ellipsis.
  Keys are shown verbatim. They are never rewritten the way hl turns `_` into
  `-`.
- A row whose level is error or fatal gets a 2 px left border in `danger`. There
  is no full-row fill.
- **Unparsed lines are rows, not dropped.** They get a `RAW` badge and count
  toward "N unparsed lines" in the status bar. Clicking that count filters to
  them. This applies "ambiguity surfaced" to format detection.
- A record without a level shows `---`. The level filter has an explicit
  checkbox, **"Include records without level"**, which defaults to **on** when
  the file has no level field at all, and the bar says so. This avoids hl's
  silent "0 lines" result on Tessera's own log (§1.6).

### 3.3 Filter bar and query

- **Level dropdown:** ≥ trace/debug/info/warn/error, or an exact multi-select.
- **Time range:** presets (last 5/15/60 min relative to the *last* record in the
  file, not to now), a custom since/until, and "around selected record ±N s".
  The time zone selector (UTC/local) is in the status bar.
- **Query field:** a subset of hl's syntax (hl `src/query.pest`), so users can
  move between the two:
  - `and`/`or`/`not`/`( )`
  - `= != < <= > >=`
  - `~=` contains, `~~=` regex
  - `in (...)`, `exists(field)`, `level>=warn`
  - a `?` include-absent flag
- hl's `in @file` form (which reads a set from disk) is **not** supported. A
  query should not read arbitrary files.
- Parse errors are shown inline with a caret, and the last good filter stays
  applied.
- **Field chips:** the `[=]`/`[≠]`/`[≥]` buttons in the detail pane add chips.
  Chips and the query field are combined with AND. Clicking a chip's ✕ removes
  it.
- Filtering runs on a background thread over the index and produces a
  `Vec<u32>` of matching row ids. The list swaps it in when ready. The
  "480,974 → 1,203" counter is the positive control that the filter actually
  ran.

### 3.4 Follow / tail

- The `● Follow` toggle in the header pins the view to the bottom. Scrolling up
  unpins it, and a floating "↓ 37 new" pill re-pins.
- New records are filtered incrementally.
- After truncation or rotation, a separator row is inserted:

```
│ ─────────── file rotated at 14:02:11 (previous content kept above) ─────── │
```

### 3.5 Multiple files

Opening several files (CLI or multi-select) merges them chronologically, using
the per-file timestamp index (hl's `-s` idea), with a narrow per-file color
stripe on the left:

```
│▌a 00:00:00.240  ERR  vault.scan  scan complete …                       │
│▌b 00:00:00.241  INF  http        request finished …                    │
```

Records without a timestamp keep their file order and are attached after the
preceding timestamped record.

### 3.6 Big files

- **Up to about 1 GB:** memory-map the file (memmap2 is already in the lock
  transitively) and index it in parallel chunks.
  - Rows appear as soon as the first chunk is indexed. A progress bar
    ("indexed 412 MB / 1.0 GB") sits in the status bar.
  - The prototype indexed 100 MB in 0.23 s on 4 threads, so this should feel
    instant up to a few hundred MB.
- **Index size:** shrink it from the prototype's 48 B to about 24 B per entry:
  `u64` offset, `u32` length, `i64` timestamp delta, and a `u8` level.
  A 10 M-record file is then about 240 MB. Above a configurable cap, show the
  *last* N MB (tail-first) with a "load earlier" control. Never fail silently.
- **Compressed inputs** can't be addressed by byte offset. Decompress once into
  an anonymous temp spool (not inside the vault or the index dir), then treat it
  like a plain file. hl's gzip run took 0.51 s against 0.30 s for the plain
  file.
- **Windows:** do **not** memory-map a file that is being followed. A mapped
  view prevents the writer from truncating or rotating it. Use shared-mode reads
  (`FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`) instead.

## 4. License, notices, size, platforms

### License and notices

- hl is MIT (`Copyright (c) 2020 Pavel Ivanov`). MIT is on the `about.toml`
  `accepted` list.
- A license scan of hl's full dependency set found only licenses that are
  already accepted. The compound ones are `encoding_rs`
  (`(Apache-2.0 OR MIT) AND BSD-3-Clause`), `ring` (`Apache-2.0 AND ISC`, a
  build-time dependency) and `unicode-ident`.
- Under the recommended option (c), no hl code is copied, so no notice is
  required. Credit hl in `docs/` as the design reference for the query syntax
  and field aliases.
- If any hl source is ported verbatim later, for example its level-alias tables
  or timestamp heuristics, that is a reviewed, issue-tracked step:
  - add `licenses/hl-MIT.txt`,
  - list it in `licenses/README.md`, so that `scripts/third-party-notices.py`
    puts it into `THIRD_PARTY_NOTICES.md`.

  cargo-about only sees crates, not ported code.
- Under (b), the hl binary needs its own MIT notice plus notices for its roughly
  250 runtime crates. That means running cargo-about against hl's lockfile.

### Compression without new C code

| Format | Crate | Status in Tessera |
|---|---|---|
| gzip | `flate2` 1.1.10 (zlib-rs backend) | already locked |
| zstd | `zstd` 0.13.3 (C `zstd-sys`) | already locked |
| bzip2 | `bzip2` 0.6.1 (pure-Rust `libbz2-rs-sys`) | already locked |
| xz | none | open question: a pure-Rust decoder (needs evaluation) or C `liblzma` as hl uses. Defer to a late slice |

### Binary size (stripped Linux x86-64; relative, same session)

- (a) +~8.8 MB
- (b) +9.4 MB, as a separate file
- (c) +~0.16 MB, measured on the prototype

Tessera's release profile has no LTO, so these are upper bounds for (a) and (c).

### Platforms

- **Linux:** inotify through notify 7; nothing special.
- **macOS:** notify uses FSEvents. Follow mode for one file should watch the
  parent directory, because FSEvents is directory-based. hl uses kqueue on macOS
  with `notify`'s `macos_kqueue` feature. Under (b), the hl helper would need
  signing and notarisation inside the bundle, alongside `Sparkle.framework`
  (`scripts/build-macos-ci.sh:43`).
- **Windows:**
  - Use shared-mode reads and don't map a followed file (§3.6).
  - Detect rotation by file ID.
  - Under (b), add the hl executable to the Velopack payload
    (`docs/windows-delivery.md`).
  - Windows ARM64 needs hl's `native-tls` build-dependency variant, another
    reason against (a).

## 5. Recommendation and slice plan

**Recommendation: (c).** Tessera gets an in-house, read-only, structured log
viewer, with hl as the design reference and a compatible subset of its query
syntax. Don't embed or bundle hl.

Each slice is one issue and one PR. Estimates are focused engineering days,
including tests.

| # | Slice | Scope | Estimate |
|---|---|---|---|
| 1 | Core index | `tessera_core::log`: detect JSON/logfmt/plain, `LogEntry`/`LogIndex`, parallel chunked indexing over a mapped file, unparsed-line handling, lossless field extraction. Fixtures include Tessera's own diagnostic-log shape. Add a `#[ignore]` 100 MB throughput test with a known-count positive control | 3–4 d |
| 2 | Log view (single file) | Accept `.log/.jsonl/.ndjson/.logfmt` in `OpenIntent::validate_inner`; log branch in `prepare_first_with_last_document`; `reader_log.rs` with `uniform_list` rows, level badges from `Palette`, detail pane, copy raw line, status bar | 4–5 d |
| 3 | Filters | Level filter (including the "without level" toggle), text contains, time range presets, field chips from the detail pane; background filtering with a match counter | 3–4 d |
| 4 | Query language | hl-compatible subset parser in core (hand-written or `pest`; prefer hand-written, no new dependency), inline errors | 3 d |
| 5 | Follow / tail | `log/tail.rs`: append, truncate and rotate on Linux/macOS/Windows; pin-to-bottom UX; network-root polling | 2–3 d |
| 6 | Vault + own log | Mount `LogView` from `Reader::preview_file` for log attachments; **Help → Open diagnostic log** (current + `.previous`); add `level` to `reader_diagnostics.rs` records | 2 d |
| 7 | Compressed input | gz/zstd/bz2 via locked crates, temp spool; xz decision | 2 d |
| 8 | Multi-file merge | Chronological merge with per-file stripes, CLI multiple paths for logs only | 3 d |
| 9 | OS integration + docs | Open With registrations (desktop file, `CFBundleDocumentTypes`, Windows ProgID), `docs/log-viewer.md` | 1–2 d |

Total: about 23–28 engineering days. Slices 1 and 2 give a usable viewer, so
the earliest demonstrable result comes after about 1.5 weeks. Slices 3–9 can be
reordered by demand. Slice 6's diagnostic-log `level` change is independent and
can go first.

## Open questions

1. **Product scope.** Is a log viewer within the PRD's reader contract
   (`docs/PRD.md`), or a separate surface? This doc assumes it is a read-only
   reader mode and does not change the PRD. The owner should confirm before
   slice 2.
2. **CLI multi-path.** `parse_args` currently allows one positional path ("Open
   one file or folder per CLI invocation"). Is multi-file merge (slice 8) worth
   relaxing that for logs only?
3. **xz support.** Is it needed at all? If yes, should it be pure Rust (to be
   evaluated) or C `liblzma`?
4. **Size cap.** Default cap for in-memory indexing, and whether tail-first
   loading is acceptable for files above it.
5. **Upstream hl.** File an issue suggesting a stable library API (public
   `Record`, no network in `build.rs`)? It costs little, but nothing here waits
   on it.

## Reproducing the evidence

Everything ran in a scratch directory outside the repo:

```sh
git clone https://github.com/pamburus/hl.git && cd hl     # c1ac299f, v0.36.3
cargo build --release --locked                            # 2m57s on 4 vCPU
# synthetic inputs: 100 MiB JSON lines + logfmt, seeded RNG (602)
hl -P app.jsonl ; hl -P -l e app.jsonl ; hl -P -s app.jsonl ; ...
grep -c '"level":"error"' app.jsonl                        # 23873, positive control
```

The probe crates (`hl` as a path dependency) and the option (c) prototype are
described in §1.2 and §1.7. They are deliberately not committed: this branch
carries research only.

## Implementation notes (slices 1–2)

Decisions taken while building `tessera_core::log` and `reader_log.rs`. They
are open to review; none changes the recommendation above.

- **Mapping.** Files up to 32 MiB are read into memory and larger ones are
  memory-mapped (`memmap2`, already in `Cargo.lock`). Reading keeps the usual
  small log, including Tessera's own 4 MiB-bounded diagnostic log, safe from a
  writer that truncates it while it is shown; a truncated *mapped* file would
  fault on access. Follow mode (slice 5) must not map the file it follows.
- **Size cap.** Files above 2 GiB are refused with a message (open question 4
  above). Nothing is truncated silently.
- **Index entry.** 32 bytes: offset, timestamp, line number, length, level.
  Blank lines are not entries but keep line numbering exact.
- **Formats.** The verdict comes from the first 256 non-blank lines (at most
  64 KiB, never cutting a line). logfmt requires every token to be `key=value`,
  so prose such as `Starting server at port=8080` is an unparsed row rather than
  a record with invented boolean keys. `Mixed` (JSON and logfmt, each at least a
  tenth of the sample) parses each line by its first byte.
- **Levels.** A record without a level field is `Missing` (`---`), an unmapped
  value is `Unknown`, and a line that is not a record is `Unparsed` (`RAW`).
  Nothing is inferred: Tessera's diagnostic records with an `error` key stay
  `---`, and the status line says "No record has a level". Numeric levels follow
  bunyan/pino (10–60); other numbers are `Unknown`.
- **Timestamps.** RFC 3339 plus space separator, comma fraction and `±HHMM`
  offsets; epoch numbers by magnitude (s/ms/µs/ns), decimals computed exactly.
  A value without an offset is read as UTC, which displays it as written. The
  highest-priority time alias decides; an unparseable one is "no timestamp", not
  a fallback to another field.
- **Fields.** Lossless: original key spelling, source order, duplicates kept,
  numbers as written (`812.50`), nested objects flattened to dotted keys, arrays
  as raw JSON.
- **Reader.** A log always opens in the quick viewer, even inside an open
  folder. The log is the remembered document of that quick viewer, so a restart
  reopens it. Sibling logs clicked in the quick viewer's tree are indexed off
  the UI thread; logs inside a vault keep the file card until slice 6.
- **Measured** on a 4 vCPU Intel Xeon @ 2.10 GHz VM with 15 GiB RAM, release
  build, warm cache. That is the same VM shape as §1.6/§1.7 but a different
  session, so it is not a like-for-like comparison with those numbers:
  the `#[ignore]` probe (`cargo test -p tessera-core --release --lib
  log::tests::throughput -- --ignored --nocapture`) indexed a 100 MiB seeded
  JSON-lines file (531,440 records) in 0.18 s on 4 threads and 0.41 s on one,
  with a 16.2 MiB index. Its counts match both the generator and an independent
  byte search.
