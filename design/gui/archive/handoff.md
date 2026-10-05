# Tessera GUI design handoff — V2

Status: revised design proposal delivered, 2026-09-06. Synthetic browser prototype only; no native application change or native acceptance claimed. Oleg responded positively to this visual direction: “now we’re talking!”.

## Recommended interface

A three-pane desktop workspace inspired by the supplied 1Password reference: neutral sidebar, persistent goal/source list, spacious selected-item view and a global search/capture toolbar. Goal discussion, execution, outcomes and source details belong to that same selection. The message composer stays visible while conversation content scrolls.

Buttons use a consistent 8px radius, 36–38px comfortable height and explicit hierarchy: blue primary, soft neutral secondary, quiet toolbar and separate destructive actions. UI icons are consistent inline SVG. Light mode uses white/neutral surfaces; dark mode uses charcoal/slate. Source conflicts compare current and draft side by side on desktop, with visible resolution actions; narrower layouts stack them.

## Review artifacts

- [Interactive prototype](https://html.me.uk/t/tessera-gui-2026-09-06/prototype.html) · [short link](https://oklb.uk/merry-badger).
- [22-screen gallery](https://html.me.uk/t/tessera-gui-2026-09-06/screens/index.html) · [short link](https://oklb.uk/sleek-raven-3148).
- [Download V2 editable source](https://pomoi.co/f/64d42448-7195-4c22-bf73-04d018494497/tessera-gui-v2.zip) · [short link](https://oklb.uk/rapid-tiger). ZIP SHA-256 `1acc4fa83d8e7b86389383983b74fc94461a9ee718f8a1a51b1e27cbecf4be6e`.
- [Editable source entrypoint](../README.md), [demo script](../demo-script.md), [information architecture](../information-architecture.md), [interaction/button specification](../interaction-spec.md), [implementation mapping](../implementation-map.md).
- [Browser/visual verification](../verification.md), [brand and GUI alias rationale](../brand-requests.md), [publication receipts](../publication.json).

The prototype and screenshots replace the first visual proposal. Published files expire 2026-10-06; committed sources remain durable.

## Requested outcome and provenance

Original [GUI design brief](https://pomoi.co/f/843b1e2c-1e14-4a4d-bee7-144271390777/agent-2-gui-design.md), followed by Oleg's explicit request to redo the appearance using his 1Password screenshot, including the buttons. The screenshot supplies layout/control inspiration, not Tessera branding, product data or capabilities; it is not redistributed in the synthetic package.

- Worktree: `/home/example/worktrees/tessera/app-gui` on verified development host.
- Branch: `design/tessera-app-gui`; [PR #100](https://git.oklabs.uk/BeFeast/tessera/pulls/100), [issue #99](https://git.oklabs.uk/BeFeast/tessera/issues/99).
- Recorded branch base: `a68a7d15fa435a06445641d4aabb6d96994fc112`; first proposal retained in git at `1294225`.
- Ownership: only `design/gui/`. Shared checkout, production app, brand source, vendor patches, linux-reference-host desktop and provider/service state remain untouched.
- Tier: P2 visual redesign, with existing P1 mock behavior regression checks.

## Tokens and typography

Immutable [brand 1.1.0](../imports/brand-1.1.0.json) supplies the Noto Sans/Cascadia Code families. Latin/Cyrillic WOFF2 assets are embedded in the self-contained HTML; Chromium confirms actual custom fonts. [Font provenance](../prototype/font-provenance.json).

GUI-owned [interface aliases 2.0.0](../interface-tokens.json) and [layout tokens](../layout-tokens.json) implement the user's later neutral 1Password-inspired direction without editing `design/brand/`. These explicitly supersede the original brand-tinted GUI surfaces. Exact imported brand bytes match commit `ee6a73ffa6852e7d12dcc4e536dbc51b04f63bb7` on `design/tessera-brand-identity`; [full source/hash provenance](../imports/provenance.json).

[26 active GUI pairings](../imports/interface-contrast-2.0.0.json) pass stated text/primary-label/focus thresholds. This validates those pairs, not native accessibility or every possible composition; actual light/dark full screens were also inspected.

## Verification and scope

Seven grouped mock interaction checks passed; 35 viewport/surface combinations have no document-wide horizontal overflow. Twenty-two current screenshots were generated from the final prototype SHA-256 `f587f50711ede3aede84e0dd5e7db431217abb60aa1afb95a6c5f88d8b9628e0` and inspected, including 1280×800 buttons/composer, light/dark, source and conflict recovery.

Preserved interactions include capture → discussion → Todoist task → prepared T3 stage → execution/recovery → saved result → review, separate goals and prior stages, exact-source fixture editing/preview, dirty navigation, renewed conflict, connector recovery, interrupted chat and synthetic TAR export. TAR evidence verifies all eight fixture files' bytes/lengths; no real provider is contacted. Opening a retained reference does not mutate a goal's state.

Mobile capture/attention remains explicitly future scope. Prototype tools expose simulated empty/loading/error/offline and other trust states. The 640px reflow check is not a native zoom/accessibility certification. Native source/disk guarantees, backend continuity, font distribution and device/provider acceptance still require the engineering work mapped in implementation-map.md.

## Resume

All design/review workers completed their assigned revision. No native work or merge/deploy is implied by this design receipt.

waiting_on: further concrete design feedback or a separately scoped native integration task; the revised visual direction received positive feedback.
resume_when: concrete feedback or a selected implementation slice arrives.
next_safe_action: revise the named design surface or use implementation-map.md to implement the selected native slice within its own scope.
