# Large-note Reader benchmark (#653)

Open time, scroll and «On this page» jumps on one 10,000-line note, with budgets.

```sh
scripts/vendor-setup.sh && scripts/vendor-setup.sh --verify
cargo build --release --locked -p okilum-shell --bin okilum   # through the build queue, never on maestro
python3 scripts/perf/large-note/bench.py target/release/okilum --runs 3 [--json out.json] [--no-budget]
```

`fixture.py` writes the stress vault: `Large.md` (10,000 lines, 2,000 headings, 500 links
of four kinds, four 8-column tables of 250 rows, 200 image references to 20 PNGs, pinned by
SHA-256) and `Small.md`, the control note. Each run gets its own HOME, XDG and state
directories, index and vault copy; Xvfb starts when no `DISPLAY` is set.

The Reader probe (`crates/okilum-shell/src/reader_perf_probe.rs`) does nothing unless
`OKILUM_READER_PERF_PROBE` names a report file. After the first frame with the note's blocks
it scrolls 240 frames of 96 px, opens four far «On this page» rows (50 %, 95 %, 25 %, 75 %)
as a click does and holds each for 8 frames, writes JSON and quits. Frame time is the Reader's
CPU time from `Reader::render` to the end of the body's paint; presentation (software
rasterisation under Xvfb) is not included.

| metric | what | budget |
|---|---|---:|
| `open_ms` | launch to the first painted frame of the note | 3000 |
| `open_over_control_ms` | `open_ms` minus the small note's, opened the same way | 300 |
| `document_stage_ms` | `first_event_received` → `document_first_paint` in the diagnostic log: the TextView parse and publication of the note, without launch, scan and display start-up | 400 |
| `first_frame_ms` | the first document frame | 100 |
| `scroll.p95_ms`, `scroll.max_ms` | scroll frames | 25, 50 |
| `jump.p95_ms`, `jump.max_ms` | frames after an outline jump | 25, 60 |

A run is rejected, never counted, when a control fails: the note's blocks painted, the outline
has every heading, scrolling moved, each jump put its heading's block on top, the control note
opened. Budgets are for the median of the runs on a 4–6 vCPU Linux host under Xvfb; compare
numbers only from one machine in one session. Tighten, never loosen, without review.

## Theme A/B on code blocks (#3156)

`Code.md` (written next to `Large.md`, pinned too) has 10,000 lines: 434 fenced
code blocks in Rust, Python, JavaScript and JSON under 435 headings. Compare a
theme with its own syntax palette against one on the toolkit palette, with
the same binary in one session, alternating:

```sh
for theme in graphite obsidianite obsidianite graphite; do
  python3 scripts/perf/large-note/bench.py target/release/okilum --note code \
    --theme $theme --appearance dark --runs 3 --no-budget --json $theme-$RANDOM.json
done
```

Each launch starts with that theme and mode in its `reader-ui.json`; the probe
reports the theme, mode and syntax palette it drew with, and a run in another
one is rejected.

## Before and after (okilum-dev, 6 vCPU, one session, 17.11.2026)

Main `59c1fe26` against this change (vendor patch 0149), built in one build-queue slot,
interleaved base / change / change / base, 3 launches each, medians of 6:

| metric | main | chunked parse |
|---|---:|---:|
| `document_stage_ms` | 710 | 194 |
| `open_over_control_ms` | 685 | 155 |
| `open_ms` | 1547 | 1035 |
| `first_frame_ms` | 17.3 | 14.8 |
| scroll p95 | 5.2 | 5.1 |
| jump p95 | 8.8 | 8.7 |

Main fails the `document_stage_ms` and `open_over_control_ms` budgets in every set; the
change passes all budgets. Scroll, the first frame and outline jumps were already within
budget on main (#1003 lazy measuring, #1004/#1290 the outline list and scroll work).

## Where the open time went

Measured in the same slot: markdown-rs parses the whole 435 KB note in 355 ms and its first
1,250 / 2,500 / 5,000 lines in 8 / 38 / 103 ms, so parse time grows faster than the note.
The TextView parsed it whole on a background thread before the note could be published.
Patch 0149 (`scripts/patches/0149-text-markdown-parse-long-documents-in-chunks.diff`) parses
documents of 32 KiB and more as ~16 KiB chunks on up to four threads and joins the trees with
their positions shifted. A cut is made only before a column-0 ATX heading after a blank line
outside a top-level fence; definitions, footnotes, HTML blocks that span blank lines,
indented or container fences, frontmatter, math and MDX keep the single parse. Tests in the
patch check that the chunked tree equals a single parse (LF, CRLF, CR) and that unsafe
documents are not chunked; `scripts/ci/live-preview-tests.sh` runs them.
