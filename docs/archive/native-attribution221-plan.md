# Native #221 attribution plan

> **Historical document.** Written before Tessera was renamed Okilum (2026-10-09). Names, paths and links are kept as they were then.

Base: `d2c181ca7a998effdee82e3c1486f8fca8de3d0a` (merged #217). This is a bounded follow-up to the accepted V3 native prerequisite, not #69 benchmarking or #211 acceptance. The V3 987-byte fixture produced Source/LP first coherent trailing-marker medians of 10.326/18.370 ms; top-level submission medians were 17.875/19.9065 ms. Neither difference identifies a function or physical presentation.

## Ownership and boundary

- #221 owns this isolated worktree, native example instrumentation, and opt-in vendor instrumentation represented by an additional reproducible patch. GPUI core stays unchanged.
- #220 owns production shell code. Its adapter is moving to `src/brain/source_projection.rs` and scheduler to `src/brain/source_projection_ui.rs`. Instrument behavior at the example/vendor boundary; do not edit those production files or introduce conflicting adapter changes.
- Agent 48 owns builds and the shared build lock. Agent 204 remains sole linux-test-host GUI owner. Root and independent reviewer 200 receive this plan before any build or native run.

## Instrumentation

Use one opt-in process-local bounded event buffer, monotonic timestamps, explicit overflow counts, and an explicit dump outside measured action windows. No per-span JSON formatting, file writes, source hashing, or source serialization in timed paths. Existing legacy markers remain selectable so their effect can be measured. Record source document/generation, presentation epoch, editor identity, span/parent IDs and thread identity where available; carry correlation across async tasks rather than treating background work as UI time.

| Boundary | Record | Attribution supported |
| --- | --- | --- |
| Fixture schedule and adoption | enqueue, background start/end, UI resume, publish start/end, accepted/rejected generation | executor wait, actual classification task, adoption work and delay |
| Vendor replacement and refresh | spans around `reveal_replacement`, `refresh_projection`, provider compose; reason, authority/active stamps, success/fallback | repeated composition, replacement versus adoption/metrics refresh; stale returns remain visible |
| Projected wrapping | `set_projected` elapsed, logical lines/bytes/style runs, shape-call count and aggregate duration | complete-buffer projected wrapping versus time inside GPUI text-system calls; no unobserved cache-hit claim |
| Visible layout | `layout_lines` elapsed, visible fragments, shape-call count and aggregate duration in both Source and LP | visible shaping/layout with matched Source control |
| Actual editor prepaint/paint | prepaint start/end, actual captured layout source/epoch, paint start/end, current source/epoch at paint | actual component paint separated from the fixture trailing marker; stale/coherent classification based on the layout used, not only current state |

Do not add derived full-text copies to obtain stamps. Preserve the exact layout identity used by the component through prepaint into paint. Background and nested spans are not additive: report inclusive durations and exclusive UI durations separately, and leave unattributed gaps labeled as gaps. No optimization or changed invalidation behavior in the attribution patch.

## Minimal native controls

One session on linux-test-host, original 987-byte fixture, same fonts and 1100 × 850 client, normal clipboard delay, no IME helper, same sparse edit cadence. Preserve quiet qualification, exact binary/source pins, service baseline/cleanup and GPU fault checks from V3.

1. Legacy control: spans off, legacy trailing marker on, WAYLAND_DEBUG on.
2. Attribution: buffered spans on, legacy trailing marker on, WAYLAND_DEBUG on.
3. Marker control: buffered spans on, legacy trailing marker off, WAYLAND_DEBUG on.

Each configuration gets 12 edits per mode in matched six-edit Source/LP/LP/Source blocks. Reset to the exact 987 bytes and fresh history before EACH six-edit block, after untimed warmup; verify the baseline hash and selection. Each block therefore grows through 988–993 bytes. This differs from V3's 988–1017-byte sequence and must not be presented as a direct replication of its median. The same frozen binary selects each configuration. Compare legacy milestones between (1) and (2) to bound instrumentation overhead; compare actual component paint/submission between (2) and (3) to identify trailing-marker effects. WAYLAND_DEBUG remains matched in all arms; these runs cannot attribute its absolute overhead. If overhead is material, reduce instrumentation and repeat only the affected controls before interpreting function costs.

Positive controls: one F7 and one resize without source mutation must produce presentation/layout evidence; one actual edit must produce a source mutation and actual component paint with the new generation. One legacy coherence mismatch must be checked against actual component layout identity. Capture quiet idle frame/span counts to establish that absent edit spans are detectable. Exclude all control/setup windows from timing samples, ending each window at the next ANY controller action as in the corrected V3 parser.

## Reviewable result

Deliver per-edit correlated timelines and call counts, medians/ranges for this bounded corpus, separate actual work versus inter-frame gaps, instrumentation/marker control deltas, and exact source correctness plus cleanup receipts. Retain raw events, parser, configuration and hash manifests. Twelve samples per mode are diagnostic, not a p99/tail or parity claim. A recommendation for reuse or duplicate suppression requires measured dominant work and preservation of the native #218/#219 semantics; implementation is a separate reviewed change.

## Implemented event contract and limitations

`TESSERA_NATIVE221_SPANS=1` enables a preallocated 65,536-event buffer. With spans disabled, each probe still has argument/field loads, a function call and relaxed atomic branch; shape probes also take that disabled branch. It performs no clock calls, TLS access or buffer locking. The same-binary control bounds enabled instrumentation overhead, not the residual disabled branch overhead relative to an uninstrumented binary.

`TESSERA_NATIVE221_LEGACY_MARKER=0` suppresses only the legacy trailing canvas. Keep `TESSERA_NATIVE216_TIMING=1` in all measured arms for source markers and clock calibration. F6 resets the exact fixture/history; F9 dumps and clears completed events. A dump includes monotonic start/end; a following `attribution_dump_complete` marker is emitted after stdout serialization. Wait for classifier/UI quiescence before dumping. The parser must reject measured samples overlapping a dump, spans crossing its boundary, and incomplete parent relationships. Any overflow invalidates the entire dump interval, even when retained events appear complete.

Events carry `id`, same-thread `parent`, `thread`, start/end, source identity and optional current identity. `projected` is the observed component state; it is null for fixture scheduling/publish, which cannot inspect it. Actual paint identity comes from the prepaint `LastLayout`; `current` is sampled at component paint entry. The classifier job ID is its enqueue span ID; background work and UI resume carry it in `counts[0]`. No live span crosses an await or changes thread.

`source_mutation.outcome` is edit/preedit/silent/reset. `provider_compose.outcome` is no_provider/provider_rejected/source_mismatch/accepted; its counts are anchor byte, head byte, composition-present and replacement-present. Replacement counts are start/end/source length/unused. Projected wrapping counts are bytes/logical lines/style runs/unused. Visible layout counts are display bytes/visible logical lines/style runs/unused. Component prepaint/paint counts are visible logical lines/layout groups/unused/unused. Shape-call counts and nanoseconds are aggregates within their containing wrapping/layout span, not separate events or verified GPUI cache misses.

Span end time is sampled before buffer locking/push. Observer lock/push overhead can appear inside an enclosing span or an unattributed gap, and is not included in the child duration. Exclusive UI calculations subtract the union of nested spans on that same thread only; they cannot be labeled pure business-code costs. Background overlap must never be subtracted from UI spans. Epoch zero on `fixture_publish.identity` means unavailable; provider result authority and component adoption/layout epochs remain separately recorded.

## Projected grapheme-boundary lookup

Managed size acceptance observed three 64 KiB heading `set_projected` spans of
515–521 ms, with only 22–26 ms inside their two `shape_text` calls. The old wrap
conversion restarted grapheme segmentation at byte zero for each shaped boundary.
The following layouts contained 537–538 retained wrap boundaries on the same long
line. These opportunistic spans identify a repeated traversal candidate; they do
not assign the entire remaining interval to segmentation or establish a benchmark.

Patch 0017 constructs exact grapheme boundaries once per wrapped logical line,
only during that `set_projected` call. Left and Right clipping use the same local
table. Unwrapped lines do not build a table. Existing out-of-range rejection,
CRLF midpoint behavior, EOF, nonadvancing Left-to-Right retry, and final range
guards are preserved. Individual hit testing retains its original helper; no
source replacement, font cache, persistent layout state, or GPUI core changes.

Focused differential tests compare every byte offset and both biases against the
original helper, including offsets inside scalars, combining sequences, ZWJ,
bidi text, empty text, CRLF, EOF, and invalid offsets. Final ranges are compared
for empty, repeated, and nonmonotonic raw boundaries; a comparison-count test
checks growth of segmentation and lookup work. Existing `set_projected` and
`shape_ns` spans suffice for a paired native comparison, so no extra probes are
added. Native latency and geometry acceptance remain separate gates; no measured
speedup is claimed by these source changes or tests.
