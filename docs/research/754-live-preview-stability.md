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

## Owner correction and priorities (2026-10-08)

The owner confirms that Live Preview is stable at idle. The idle-cycle/blink hypothesis is no longer the leading P1: focus on editing/caret reveal → rewrap, scroll anchoring when block heights change, and save/iCloud echo. The markdownlivepreview screenshot is a render-only visual sample, **not an interaction reference for a live editor**. #755/#756 retain their value as measurement tools, not an independently confirmed idle defect.

Own-save echo remains a hypothesis for the owner's experience, not a proven editor reset: three same-byte batches through the actual incremental-publication path preserve editor identity, source stamp, accepted provider, presentation epoch, selection and scroll. A different-byte external update is the positive control. Keep `editor_disk_refresh` observations in the Mac probe; do not suppress real external changes or index updates.

## How Velotype / SoloMD handle this

This is a pinned source audit, not native acceptance of these products; no upstream code was copied into Tessera.

**Velotype — GPUI0.2, Apache-2.0.** [Cargo.toml](https://github.com/manyougz/velotype/blob/ed65977be94f2f2703037fcb8b6cbab2e7579571/Cargo.toml) and [LICENSE-APACHE](https://github.com/manyougz/velotype/blob/ed65977be94f2f2703037fcb8b6cbab2e7579571/LICENSE-APACHE) agree (GitHub's generic license API returns NOASSERTION, so the actual files were checked). Apache code can coexist in an MIT product if its license/notice/change obligations are retained; it cannot simply be relabelled MIT. Any selective port still needs file/dependency provenance review. For this task only architectural ideas are used.

- [Inline projection](https://github.com/manyougz/velotype/blob/ed65977be94f2f2703037fcb8b6cbab2e7579571/src/components/block/runtime/projection.rs#L252) is a temporary view over clean inline fragments with clean↔display maps; it expands delimiters only for fragments touched by caret/selection/IME, rather than revealing an entire paragraph just because the caret enters it. This reduces the scope of reflow; it does not mathematically eliminate it.
- The editor owns distinct block entities and shaped block elements with their own metrics, rather than one fixed-height gpui-kit WrapMap. [Rendering](https://github.com/manyougz/velotype/blob/ed65977be94f2f2703037fcb8b6cbab2e7579571/src/editor/render.rs#L1770) caches measured row strides keyed by block entity; invalidates on column-width changes; skips index-based refresh after structural changes; uses top/bottom spacers and keeps the focused row mounted. Variable-height blocks are part of the model.
- [Caret visibility](https://github.com/manyougz/velotype/blob/ed65977be94f2f2703037fcb8b6cbab2e7579571/src/editor/render.rs#L433) uses actual caret/block bounds and pixel scroll correction after layout, with a pending recheck; it avoids automatic correction while dragging the scrollbar. This is caret visibility management, not proof of a full source-position viewport-anchor guarantee.
- Its clean-fragment/block/source-mapping model differs from Tessera's single canonical exact-source buffer. A transplant of the whole editor would endanger our lossless source/Undo/IME contracts; the useful ideas are local projection invalidation, stable block identity, measured heights and coherent pointer coordinates.

**SoloMD — MIT, Tauri + Vue/CodeMirror6 frontend, Rust backend.** [LICENSE](https://github.com/zhitongblog/solomd/blob/154b4723d2c646a367502772fa372a9306430bbc/LICENSE), [frontend dependencies](https://github.com/zhitongblog/solomd/blob/154b4723d2c646a367502772fa372a9306430bbc/app/package.json). The vault note's “entirely native Rust editor” description is not accurate for this repository.

- [cm-live-preview.ts](https://github.com/zhitongblog/solomd/blob/154b4723d2c646a367502772fa372a9306430bbc/app/src/lib/cm-live-preview.ts#L1) retains the canonical Markdown buffer and applies decorations. It reveals marker ranges on selection-touched **source lines**, styles headings at1.7/1.4/1.22/1.1em, and rebuilds on document/viewport/selection events, not blink. Current code hides heading/emphasis/strike markers but deliberately keeps link brackets and code backticks as affordances; this is narrower than the introductory prose suggests.
- [Drag guard](https://github.com/zhitongblog/solomd/blob/154b4723d2c646a367502772fa372a9306430bbc/app/src/lib/cm-drag-aware.ts) freezes selection-only decoration changes during a real drag and flushes after release, with blur/cancel recovery. [IME guard](https://github.com/zhitongblog/solomd/blob/154b4723d2c646a367502772fa372a9306430bbc/app/src/lib/cm-ime-guard.ts) freezes and **maps decorations through edits**, then rebuilds after composition. These are directly relevant behavioral patterns for #754 and our IME gate.
- The source explicitly documents an “Always show Markdown markers” option: retaining/dimming markers avoids caret-triggered reflow. Therefore default reveal does not itself guarantee zero jumps. CodeMirror/browser layout supports variable heights; its implementation is not a GPUI patch.

**Ferrite — MIT, egui + comrak.** [LICENSE](https://github.com/OlaProeis/Ferrite/blob/3ba085c561670342d72c560efbf6b0b92b5c0b46/LICENSE), [editor](https://github.com/OlaProeis/Ferrite/blob/3ba085c561670342d72c560efbf6b0b92b5c0b46/src/markdown/editor.rs), [native Mermaid](https://github.com/OlaProeis/Ferrite/blob/3ba085c561670342d72c560efbf6b0b92b5c0b46/src/markdown/mermaid/mod.rs). It uses a rendered block editing session (headings/paragraphs/lists and separate editable table cells), caches block heights by source slice and render parameters (width/font), and progressively measures/culls blocks. This is a useful S7 interaction/reference architecture, not a continuous single-buffer projection implementation. Native Mermaid exists, but completeness/diagram parity and embedding cost must be evaluated separately; no imported renderer is approved here.

## Revised S3–S7 comparison (live-editor references)

| Slice | Reference lesson | Tessera scope / constraints |
| --- | --- | --- |
| S3 block-local classification | Velotype's block identity/cache; SoloMD's mapped decorations through IME | Pull forward unchanged-block retention and dirty-block-only adoption to remove whole-note raw flashes. Do not blindly reuse stale byte maps. |
| S4 incremental mapping | Velotype clean/display mapping; SoloMD change-mapped ranges | Preserve exact source positions, composition and gesture anchors; local invalidation first, SumTree scaling remains the planned implementation. |
| S5 replacement runs | SoloMD marker/bullet decorations, Velotype fragment-local editing | Prototype line/fragment-scoped reveal and freeze/remap during drag/IME. S5 image icon is not a full image block. |
| S6 variable metrics | Velotype measured block strides and caret pixel visibility; SoloMD heading font sizes | Real heading sizes require measured row/block heights, pointer/IME geometry and source-anchor scroll correction. Fixed **row** height never guaranteed a fixed **row count**. |
| S7 block replacement | Ferrite click-to-edit heading/list/table cells; Velotype block editors; SoloMD block widgets | Images, tables, embeds/callouts need measured-height blocks and explicit edit transitions. Mermaid is a proposed separate renderer integration, not already promised by S7. |

Lists/blockquote styling and nesting were listed as Later in the approved roadmap; these references justify discussing a dedicated earlier slice, not silently changing that plan. Keep the MIT product/license decision and gpui-core-unpatched rule.

Recommended order: #754 interaction stability (local reveal/adoption + gesture safety + source-anchor compensation) → measured typography (S6 after required foundations) → S7 rich blocks. Confirm priorities with the owner; neither new features nor a different canonical document model are authorized by this comparison.
