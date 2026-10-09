# V2 three-pane GUI review

Status: **reviewed within the synthetic browser scope**, 2026-09-06. [Current 22-screen gallery](screens/index.html). This receipt and all current PNGs replace the rejected first visual proposal.

The review uses the user's supplied 1Password screenshot as the visual reference: a neutral sidebar, contextual item list and detail pane; quiet separators; a clear selected item; restrained controls and a distinct blue primary action. It does not claim the user has accepted this redesign or that native Okilum has changed.

## Visual result

Inspected actual rendered screens at **1280×800 first**, 1440×900 desktop, and 1024×768 source view, including light and dark. The new sidebar/list/detail hierarchy substantially follows the reference. Source/goal selection is tied to the middle list; display controls are grouped in the toolbar. The former blue canvas, repeated outlined cards and always-visible demo strip are gone. Buttons have consistent shape and separate blue primary, soft secondary, and quiet toolbar/destructive roles. Prototype tools are behind a popover.

The composer is docked in the detail pane: at 1280×800 its Send action remains visible while the conversation scrolls independently. The task-preparation action remains in the conversation and can require scrolling; the dock does not imply the whole conversation fits in one viewport. Sources/details are explicit tabs and toolbar navigation, rather than another permanent competing column.

The last visual review found two actual-render issues that token checks alone missed. Dark button colors briefly retained light-theme values during theme transition, obscuring navigation labels; the prototype owner removed that transition and applied explicit dark control roles. Conflict comparison stacked the full current source above the draft, hiding resolution choices; at desktop width current/draft now sit side by side in bounded scrollable regions, with actions visible. Both corrections were re-inspected in the final screenshots. At 1440×900 the current and draft regions occupy approximately y374–654 and the resolution action row starts around y710. Narrower source layouts deliberately stack.

Current screenshots include light/dark discussion, running execution, blocker, outcome/review, completed and prior stage, source/preview, dirty guard, newer conflict/current comparison, connections, export success/failure, trust-state library, offline state, compact laptop, 1024px source, 640px reflow and future mobile concept. They show actual viewport/pane content; a screenshot does not expose every line inside an independently scrollable pane.

## Reproducible evidence

- Worktree: `/home/example/worktrees/okilum/app-gui`, branch `design/okilum-app-gui`.
- Existing Node Playwright and cached headless Chromium **151.0.7922.34**, local `file://` prototype; no package/browser installation, native desktop automation or service changes.
- [Screenshot provenance](screens/captures.json): V2 identity, viewport, theme/density, platform fonts and source SHA-256 **f587f50711ede3aede84e0dd5e7db431217abb60aa1afb95a6c5f88d8b9628e0**. All 22 current images use this final redesign.
- [Interaction checks](screens/checks.json): **7/7 grouped scenarios passed**, no page errors. Initial script attempts used obsolete duplicate-nav or hidden-controls selectors; those harness failures were corrected to follow the new visible navigation and Prototype tools. They are not reported as product failures.
- [Layout results](screens/layout-final.json): **35/35 combinations without document-wide horizontal overflow**, seven destinations across 1440×900, 1280×800, 1024×768, 640×400 and 390×844. Internal lists, documents and mobile navigation may scroll locally.
- [Interface contrast receipt](imports/interface-contrast-2.0.0.json): parent measured 26 active text/muted/selected/primary/focus/link pairs, passing the stated 4.5:1 text and 3:1 focus targets. Brand-only colors are not a substitute for this neutral-interface receipt.
- Reproduction scripts: [review.cjs](screens/review.cjs), [layout.cjs](screens/layout.cjs), [capture.cjs](screens/capture.cjs). Scripts use installed tools, accept `PLAYWRIGHT_MODULE` / `CHROMIUM_PATH` overrides, and wait for embedded fonts before capture.

## Behavioral regression results

| Group | Verified observation |
|---|---|
| Capture → discussion → task/auth recovery → prepare/start → blocker/result/review → follow-up | New thought retained its own conversation; mock task required restored Todoist connection and confirmation; Start remained explicit; engine success did not verify the goal; three required review checks gated acceptance; follow-up retained prior stage and goal identity. |
| Dirty source and renewed conflict | Dirty-navigation dialog focused Keep editing; Escape preserved test draft. Simulated external change plus Ctrl+S retained draft; newer incoming removed Save resolved draft until latest comparison was reviewed; deliberate resolution cleared dirty state. |
| Keyboard search and source typing | Ctrl+K focused query, 12 Tab presses never focused background controls, Escape restored Search invoker; N typed inside source did not trigger Capture. Native dialog cycling can transiently focus body while background remains inert. |
| Interrupted discussion ownership | Switching goals did not carry the originating goal's interrupted flag into another conversation; no implicit resend. |
| Export failure → retry → download | Changed-file simulation failed explicitly; retry prepared a downloadable synthetic tar. |
| Responsive navigation, offline truth and source return | Secondary destinations remained reachable, offline did not claim Backend connected, Return to goal restored selected goal. |
| Saved-reference and tools | Selecting a saved reference opened its canonical synthetic result without mutating goal state; Escape and outside click dismissed Prototype tools. |

## Fonts and synthetic archive

Final CDP `CSS.getPlatformFontsForNode` reported **Noto Sans** as a custom embedded font for heading, subtitle and preview prose, and **Cascadia Code** for conflict Markdown. Textarea returned no glyph sample; its CSS family alone is not used as glyph proof. Preferred-font fallback from the superseded proposal is no longer the current rendering evidence. Parent separately verified the embedded font on the hosted prototype.

The [downloaded synthetic tar](screens/synthetic-brain.tar) was opened with Python `tarfile`; all eight saved entries matched manifest byte lengths and SHA-256 hashes, with `prototype: true` and `execution_restored: false`. [Archive receipt](screens/archive-check.json) records the precise file hash and byte count. This proves that this particular mock download is a readable standard archive with internally matching fixture bytes, not production locking, transport or atomic publication.

## Scope limits

The 640×400 check is a CSS reflow equivalent of a 1280×800 viewport at 200%, not an actual native zoom measurement. Full keyboard-only completion, assistive technology, native focus/zoom and GPUI behavior require implementation validation. All sources, conversations, actors, provider identities, connector states and conflicts are synthetic and reset on reload. Search filters fixtures; preview is a limited mock. Mobile is future capture/attention scope. Production dispatch, exact disk revisions, transport uncertainty, interrupted-write reconciliation, process-loss recovery and real human acceptance remain outside this design verification.
