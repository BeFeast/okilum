# Live Preview input layout: Source-wrap reuse (step 1 of 2)

Tracking: [#1679](https://git.oklabs.uk/BeFeast/okilum/issues/1679).
This step removes a redundant whole-document Source wrap before installing a
new projection. Incremental/viewport projected layout and the 33 ms native
Linux input target remain step 2. The document wall and native projection size
limits are unchanged.

Before an edit, `DisplayMap` already has a correctly wrapped Source map. Projection
adoption now keeps that map alongside the projected map, sharing the immutable
Rope/SumTree nodes. When canonical text changes, the saved Source map receives
its ordinary incremental text edit. The subsequent projected layout still wraps
the entire projected document. This avoids the first of the two full wrapping
passes; it does not make projected layout incremental.

Font, width and wrapping-indent changes update the saved Source map too. Returning
to Source or revealing an exact offset reuses the map only for identical source
bytes, otherwise rebuilding it. Whole-document replacement follows the native
text-change path. Neither the canonical buffer, history, native IME ranges nor
the retained painted-layout scroll anchor changes ownership.

## Native comparison

One foreground GUI-locked muninn session on 25 October 2026 used the pinned
`Long-LP-700.md` fixture: 62,927 bytes, 700 lines, SHA-256
`c2c85c56f2ea07b7772794dc3d63de733a0044e0dcad7c5520a74444dfc24686`.
Hyprland/Wayland, Intel i7-6600U / HD520, display 1366x768 at scale 1 / 60 Hz;
editor width 733 px. Each binary had one warmup plus five measured independent
launches. The scenario is Ctrl+Home, sparse `x`, exact copied-buffer/pixel checks,
and a 50-event wheel burst. Actual projected edit and endpoint frames are
mandatory (`raw=false`, `projection_composes>0`); a classified provider alone
would not prove this.

| Measurement | Before median / p90 | Step 1 median / p90 |
| --- | ---: | ---: |
| Input to native software postframe (ms) | 72.325 / 78.691 | 67.321 / 74.667 |
| Edit prepaint (ms) | 18.071 / 19.753 | 17.712 / 18.883 |
| Edit reshape (ms) | 11.998 / 14.508 | 10.006 / 10.372 |
| Lines reshaped per edit | 1,402 / 1,402 | 702 / 702 |
| Scroll CPU draw (ms) | 10.641 / 11.142 | 10.670 / 11.162 |

Input sample ranges overlap: this is a structural improvement, not an established
user-visible latency gain. The deterministic work reduction is 1,402 to 702 lines
on every measured edit. With five observations, nearest-rank p90 is the maximum.
Scroll reports the median/p90 of each repetition's p90 CPU draw cost. Input uses
the unchanged injection-to-postframe diagnostic endpoint, not GPU completion or
physical scanout; prepaint/reshape are reported separately and are not substitutes
for the input target.

Before binary: artifact from GitHub run `40146407269`, source
`d89c09214aff08b9e63dfb2df33f7411a738ab1a`, SHA-256
`89769c0ca1a0c70275187620c10646af06365e73caff552649af62f51a58c7ba`.
Its complete source tree equals the patch's original main base
`b4cc047c5f2457e2c115e463fed4579f5d6644eb` (tree
`641ba8da02cfc93e3765e2ae941dda9c38ba8191`).
After binary: run `40153170974`, source
`fd3ad54e0ae3423a548ec3508b6595171fbec442`, SHA-256
`3e0a174b73df3dcb2011bec9efbff290394c751942b330236b3f8ce404c8e3e4`.
The later test repair changes only the test's replacement path; the subsequent
main integration is covered by final-head required CI. These measurements are
not labelled as a binary built from that later integration commit.

All twelve repetitions passed full provider, actual projection, clipboard mutation,
pixel change and nonzero-scroll controls. Raw evidence, checksums and build receipts
are retained at `~/work/qa-evidence/1679/muninn-first/` on maestro and
`~/work/qa-evidence/1679/native-first/` on muninn. The QA apps and owned input daemon
exited and the GUI lock was released. No hosted baseline or budget was changed.

## Visible font runs: follow-up

Native prepaint previously resolved every visible logical line's fonts and colors
by scanning the projection's complete style inventory. It now uses the same
forward `StyleCursor` as projected wrapping, constructing its style index once per
prepaint and resolving each visible line against overlapping styles. Provider
order still determines overlapping font/color overrides. Sparse visible rows,
empty lines, CRLF and absolute IME underline ranges use the existing paint path.
There is no persistent cache or change to source mapping, epochs, frame requests,
row geometry or scroll anchoring. The index still covers the whole inventory;
this change does not make all per-keystroke work proportional to one block.

The regression compares the actual visible-run consumer with the previous full
scan, including unsorted overlapping styles and Unicode/IME inputs. A supported
long-note control counts actual style visits during font/color resolution;
that count excludes sorting, allocations, parsing and native frame latency.
Current-head native muninn latency and Mac/no-jump acceptance remain to be measured.
The 64 KiB app wall, 256 KiB native guard and editor latency gate are unchanged.
