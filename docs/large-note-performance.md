# Large-note Reader performance (#653)

A 10,000-line note with 2,000 headings, 500 links, four 250-row tables and 200
image references used to take about 1.8 s to open, spent about 0.7 s laying
out its first document frame, and stuttered at about 100 ms per scroll frame.
Three hotspots caused this. All three are fixed, and a benchmark with budgets
guards the result.

## Benchmark

```sh
bash scripts/vendor-setup.sh && bash scripts/vendor-setup.sh --verify
cargo build --release --locked -p tessera-shell
python3 scripts/benchmarks/large-note.py --runs 3 [--json after.json] [--baseline before.json]
```

The script generates a deterministic stress vault in a temporary directory.
It starts Xvfb when no display is set. It then opens the note in the release
Reader with `TESSERA_READER_PERF_PROBE` set, using a temporary HOME/XDG state,
so no user vault, cache or diagnostics are touched. The probe
(`reader_perf_probe.rs`) does nothing in ordinary launches. After the first
document paint it:

- scrolls one 96 px wheel step per frame for 240 frames,
- jumps to 50%, 95%, 25% and 75% of the block list, with eight frames after
  each jump,
- records Reader CPU time per frame (from `Reader::render` to the end of the
  Reader's paint; presentation and GPU time are excluded),
- writes a JSON report and quits.

`open_ms` uses the launch clock (process start to the first painted document
frame). A run is rejected unless its positive controls hold: the whole note was
painted (at least one block per heading), scrolling moved the document, every
jump landed on its target block, and the outline has every heading. On
builds that include the fixes, the outline must also have painted at least
100 px of rows. The script fails if a median exceeds a budget in `BUDGETS`.

Linux packages used headless: `xvfb`, `mesa-vulkan-drivers` (lavapipe), plus
the build prerequisites in [building.md](building.md).

## Results

All numbers come from one machine in one session: a 4-vCPU Intel Xeon @ 2.10 GHz
cloud container, Xvfb, Mesa lavapipe (software Vulkan). Each figure is the median
of 3 runs. "Before" is commit `dcac6ac` (probe only); "after" adds the fixes.

| metric | before | after | budget |
| --- | ---: | ---: | ---: |
| open (launch to first document paint) | 1786 ms | 705 ms | 1200 ms |
| first document frame | 723 ms | 17.8 ms | 100 ms |
| scroll frame p50 | 98.4 ms | 5.0 ms | - |
| scroll frame p95 | 118.8 ms | 7.4 ms | 25 ms |
| scroll frame max | 137.5 ms | 14.0 ms | 50 ms |
| frames after a far jump, p95 | 105.7 ms | 9.4 ms | 25 ms |
| frames after a far jump, max | 127.9 ms | 88.1 ms | - |

Open time is the noisiest figure: the three "after" runs measured 1112, 705
and 641 ms.

The "before" build fails the budgets for open time, first frame and scroll.
This shows the budgets can detect the regression they guard against.

Under software rendering most wall-clock time goes to lavapipe and is not
counted in frame CPU time. Its shader JIT also delays the first worker event
by about 300 ms on this host. On a GPU these phases are shorter, but this has
not been measured here. Do not compare numbers across hosts (AGENTS.md).

## Hotspots and fixes

1. **Outline layout on every frame.** Every Reader frame rebuilt and laid out
   all 2,000 Contents rows. A scroll notifies the document state, the Reader
   observes it, and the whole window re-renders. The outline is now a
   `uniform_list` that lays out only its visible rows. It sizes itself to its
   rows up to the existing height limit, so the panel looks the same.

2. **Measuring every block at open and on resize.** The TextView list used
   gpui's `measure_all`. That lays out all 4,496 blocks on the first frame
   after a document is shown, and again after every width change. Patch
   `0031-text-view-large-documents.diff` keeps `measure_all` for documents of
   up to 500 blocks, so their scrollbar stays exact. Longer documents lay out
   only visible blocks plus overdraw. The other blocks get an estimated
   height until they are measured. The patch uses only the public `ListState`
   API; gpui core stays unpatched. Positions are item-based, so heading jumps,
   Back positions and search reveals do not depend on the estimate. For long
   notes the trade-off is that the scrollbar thumb is approximate and settles
   as blocks are measured.

3. **Superlinear Markdown parsing.** markdown-rs `to_mdast` time grows faster
   than the input size. Its `edit_map::add_impl` inserts into a vector. The
   full stress note took about 480 ms, while each quarter of it took about
   53 ms. The same vendor patch splits long documents (at least 32 KiB) into
   chunks of about 16 KiB and parses them in parallel on up to four scoped
   threads. Positions are shifted back into the whole source. A cut is made
   only before an ATX heading that starts in column 0 after a blank line,
   outside a top-level fence. Such a line closes every container and every
   leaf except a fence or an HTML block of kinds 1–5. Chunking is turned off
   when the document has:
   - a fence-like line that is not in column 0,
   - link reference or footnote definitions (`]:`),
   - raw HTML block openers (`<!`, `<?`, `<pre`, `<script`, `<style`,
     `<textarea`),
   - or non-GFM constructs such as MDX, math or front matter.

   Tests check that the chunked tree equals the single-parse tree for LF,
   CRLF and CR line endings, and for documents with fences, lists, quotes,
   tables, indented code and setext headings. On the stress note the parse
   dropped from about 410 ms to about 61 ms, with an identical tree.

The Contents outline needs a full comrak parse (about 15–20 ms for 2,000
headings). That parse now runs on the background executor instead of the UI
thread, guarded by the navigation generation. Until it finishes the section
stays empty and does not say "No headings in this note".

## Known remaining cost

The first layout of a 250-row, 8-column table costs about 90 ms. This shows up
as `jump.max_ms` when a jump lands on one. Later frames reuse the shaped text.
Splitting or virtualising table rows inside one block is not part of this
change.
