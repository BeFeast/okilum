## P1: Live Preview changes geometry while typing, revealing and dragging

Owner report: Mac beta 8326, “everything jerks / moves to different positions”. Investigated on Linux using the actual native shared Editor + production CachedProvider, main a075d27, Rust 1.99, verified vendor stack; Xvfb :127, 1240×850 window, Noto Sans/Cascadia 16 px, 24 px rows. This is an isolated native widget, not a full Reader or macOS acceptance claim. No gpui-core changes.

### Reproduction matrix

Fixture: plain TOP, a paragraph with five `[short label](https://example.test/<12 wide-path segments>)` links plus bold text, plain ANCHOR / TAIL, and 30 plain lower lines. Source and scripts are attached as evidence. All coordinates below are logical pixels.

| Scenario | Native observation | Result |
| --- | --- | --- |
| Caret enters/leaves long concealed paragraph | No source edit. ANCHOR Y 311→455→311; TAIL 359→503→359; scroll remains 0. Six extra wrapped rows appear when URLs/markers reveal. TOP stays Y215 (positive control). | FAIL: 144 px reflow |
| Ordinary typing in TOP, parse-delay OFF | During 12 typed characters, captured 100 X framebuffer samples of an unchanged paragraph: 66 exactly match raw control, 34 exactly match projected control; four sampled raw/projected transitions. | FAIL: visible whole-note fallback without injected delay |
| Background reclassification, controlled 500 ms delivery delay | One character in TOP: ANCHOR 311→455→311 and raw URLs appear outside the edited block, then disappear. Delay is race amplification, not measured real parser latency. | FAIL |
| Scrolled viewport, edit plain lower line | Scroll offset stays −1166; caret content Y 1680.8→1824.8→1680.8, therefore screen Y 514.8→658.8→514.8. Text before caret expands/collapses while its byte position only changes by typed character. | FAIL: viewport anchor not compensated |
| Click visible projected label | Maps to valid canonical offset38, but reveals full paragraph and moves following rows144 px. | Logical click works; visual stability FAIL |
| Drag within already-revealed paragraph | Selection28..331; plain drag positive control selects0..20. | Baseline functioning |
| Hold pointer over plain TAIL during adoption | Selection957..957 retained, but TAIL moves503→359 while button is held. Horizontal motion at original Y then selects957..1022, extending into lower lines. | FAIL: geometry moves under pointer; no evidence of corrupt logical anchor |

Framebuffer samples are not independent presented frames and percentages are not a flicker-rate metric. The first attempted tightly batched drag did not fire and was discarded; spaced pointer delivery passed the positive control above. Offscreen `range_to_bounds` returns clamped zero-width boxes: those are excluded from anchor measurements; scrolled evidence uses caret coordinates + scroll offset instead. No claim of source corruption from these probes.

Evidence:
- Concealed / revealed: https://oklb.uk/dapper-ferret-0176 / https://oklb.uk/quiet-puffin-6952
- Pending / adopted: https://oklb.uk/fancy-pony / https://oklb.uk/gentle-robin-0024
- Pointer held, pending / adopted: https://oklb.uk/amber-beaver-7175 / https://oklb.uk/rapid-owl
- Local raw evidence: `~/.cache/tessera-qa/live-stability/` (scripts, source, telemetry JSONL, instrumentation diff, 100 cropped PNG samples).

### Cause and proposed fix order (design note; no product implementation yet)

1. `source_presentation::CachedProvider::compose_colored` returns None on any source stamp/text mismatch. Reader schedules asynchronous whole-note classification; vendor prepaint installs source until the new provider arrives. Debouncing alone would lengthen this raw interval. Keep immutable accepted block projections for unchanged blocks, map them through validated edits, reveal only dirty/uncertain blocks, then atomically apply accepted diffs. Never reuse stale byte maps blindly, especially through IME, references, fences or note switches. Pull this stability subset of S3 forward.
2. Source-preserving row height means constant height *per wrapped row*, not constant number of rows. Revealing long destinations changes width and wrap count. Prototype layout-stable reveal: pin the active block's wrap geometry across caret-only movement and render marker editing without displacing existing text (or reserve source advances consistently). Reserving width only after entering the caret row still jumps on entry, and reserving all hidden URL width permanently creates ugly gaps; neither should ship without native comparison. Preserve exact source mapping, clipboard and IME candidate geometry.
3. Before any projected-layout replacement, capture canonical source anchor + intra-row affinity + screen Y (caret when visible, otherwise first visible source position). Resolve after layout and compensate scroll in the same frame; do not race explicit wheel/scrollbar input. Clamping at document edges must be specified. This complements stable layout; it cannot fix global raw flashes by itself.
4. Pin the pointer gesture's presented-layout epoch / source anchor; queue adoption or remap coherently until mouse-up. Existing stale-stamp click deferral is insufficient because valid current-source raw and projected layouts have different geometry. Keep selection usable and prevent indefinite deferral; recheck IME, autoscroll and long drags.

### Measurable acceptance

- At fixed viewport/font/theme, caret-only reveal/conceal and local non-wrapping insertion do not move unchanged rows above caret; monitored anchor screen Y drift ≤1 logical px, except explicit user scrolling or unavoidable end-of-document clamp.
- Zero sampled raw fallback frames in unchanged supported blocks after first successful presentation, during 100 edits, at narrow/wide widths and with deterministic delayed/out-of-order classifier delivery. Positive controls must detect intentional Source toggling and actual source edits.
- No text moves under a held pointer due solely to classification adoption; forward/reverse drag endpoints resolve to expected canonical grapheme boundaries before/after adoption.
- Changing authored text enough to wrap may legitimately change the edited block's height, but viewport anchor stays within1 px; don't claim that arbitrary insertions cannot reflow.
- Source and Live Preview, light/dark, ru/he/en + CJK preedit/commit/cancel, wrapped rows, Undo/redo, exact copy/save/reopen; no bytes dropped/duplicated and no stale result accepted across note switches.
- Repeat in full Reader on Linux/Wayland and macOS before closure. Shared projection/wrap/scroll paths make the same mechanisms likely on Mac; expect different thresholds/pixel deltas from fonts, Retina scale and scheduler, not exactly144 px. Record actual Mac results rather than marking them passed from Linux.

Priority: before P2 #753, alongside #748 CI; #737 native IME gate remains separate.

## Reference versus the approved slice plan

The S3–S7 roadmap is in `359-smart-editor.md` §9; `../reader-live-preview.md` records implemented S1/S2.

| Reference feature | Planned coverage | Metric implications / gap |
| --- | --- | --- |
| Link colours, strong/emphasis/strike | S1/S2 | Implemented; stability remains #754. |
| Inline code | S1 font treatment | Reference pill/background is not an explicit slice; can be paint-only if padding does not change metrics. |
| Heading sizes and separators | S6 sizes | Different font sizes need variable row metrics; thin rules can be paint-only, but separators are not explicitly planned. |
| Bullets, nested bullet variants, roman numbering | Later lists conceal, beyond S7 | Explicit marker/numbering rendering is not in S3–S7; can preserve row height but still changes indentation and wrapping. |
| Images | S5 icon, S7 actual images/embeds | Actual image height requires block replacement, not just S5. |
| Blockquote stripe and nesting | Later blockquotes conceal | No explicit stripe/nesting slice; stripe can be paint-only, indentation changes wrapping. |
| Tables | S7 | Measured multi-column blocks conflict with current single-row mapping. |
| Code block background | Unsupported source today; no explicit slice | Background alone need not change row height; padding, fence conceal and syntax/highlight require a defined scope and mappings. |
| Mermaid | Not explicit in S3–S7 | Propose a separate S7 block-factory extension, with renderer/lifecycle scope approved first. |

S3 (block cache) and S4 (incremental mapping) are foundations, not visual parity. Constant row height is not a no-reflow guarantee: concealed widths already change the number of rows. Prioritize the stability subset of S3 now; keep S6/S7 as deliberate measured-height work. Do not treat this comparison as approval to implement the new visual scope.
