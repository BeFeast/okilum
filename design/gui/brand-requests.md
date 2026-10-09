# Brand integration and usability requests

Adopted for layout development: `okilum-design-tokens/v1`, **1.1.0 recommended**. The brand lane superseded 0.1.0 with the Panoptikon-aligned direction; recommended 1.1.0 retains the palette from provisional 0.2.0 and changes typography to Noto Sans / Cascadia Code. Historical snapshots remain immutable under `imports/`.

## Verified token pairings

The independent GUI audit checks 54 pairings across light/dark: body, muted, link and status text on canvas/surface/raised surface; selected text; primary action label; focus against canvas/surface/selected; functional border against surface. All satisfy 4.5:1 text or 3:1 focus/border thresholds. Lowest text ratio is 5.05:1. Exact pairings and method: [contrast data](imports/contrast-1.1.0.json).

These numbers validate the supplied color pairs, not every possible component composition or native accessibility implementation. `borderSubtle` is decorative only; interactive boundaries use `border`, and focus uses its separate token. Selected navigation retains a non-color indicator. Status labels always include words.

## Font availability

The host has none of the preferred Noto Sans / Cascadia Code families installed (`fc-match` returns DejaVu Sans). The final prototype embeds the brand lane’s Latin and Cyrillic WOFF2 assets so the proposed typography can be reviewed without system installation. Font hashes are recorded in [import provenance](imports/provenance.json). Native font distribution and accessibility exposure remain implementation choices. The revised interface uses role-specific 13–16px sizing with readable line spacing; Noto Sans/Cascadia Code remain the foundational families.

## User-directed revision: 1Password reference

The user rejected the first GUI proposal and supplied a 1Password screenshot as the reference, then specifically rejected the buttons. Passing token contrast did not establish a usable visual hierarchy. The first proposal overused tinted backgrounds, individually enclosed panels and prominent controls; too many elements competed with the actual goal/source content.

The revised interface uses a neutral sidebar, persistent master list and spacious white detail surface in light mode, with restrained neutral equivalents in dark mode. A single modest blue primary action establishes priority. Search and New thought live in the global toolbar; Prototype tools move to a secondary entry. Noto Sans and Cascadia Code remain. The reference informs structure and restraint; its product icons, logo and data are not copied.

**Buttons:** use one consistent sizing/radius/padding system, quiet secondary buttons, and icon-plus-text toolbar controls. Remove ubiquitous dark outline boxes and competing filled actions. Show hover, visible keyboard focus, disabled/busy and destructive states without changing layout. Keep destructive choices away from the primary happy path. A screenshot review must show the toolbar, goal action row and conflict actions together; testing one isolated button is insufficient.

**Request to brand lane:** provide or endorse neutral application-chrome roles (sidebar, master list, detail, separator, subtle selection) and a restrained blue interaction role. Current user authorization permits GUI-owned interface aliases to implement this immediately without changing the immutable brand tokens or waiting for a new palette. Brand 1.1.0 remains the typography/identity input. The implemented GUI override is [interface-tokens.json](interface-tokens.json), schema `okilum-gui-interface/v1`, version `2.0.0`: white detail/list, light neutral sidebar, charcoal/slate dark surfaces, and blue interaction accent. These overrides are separate from brand ownership and need their own rendered contrast receipt.

The previous 54-pair contrast receipt applies only to the unchanged imported colors. It does not validate revised surface/action pairings. Recheck text, selection, focus, button label and functional boundaries against the actual interface aliases in both themes, then review the rendered full screen at laptop size. Do not report this revision as accepted until the new visual review is recorded. Any subsequent recommended brand version still requires its own immutable import and renewed audit.

## Revised GUI contrast receipt

[Interface 2.0.0 pair data](imports/interface-contrast-2.0.0.json) checks 26 active text, muted, selected, primary-label, focus and link pairings in both themes. All pass their stated 4.5:1 text or 3:1 focus threshold. Muted light text was darkened to `#656b74`; dark primary fill is `#176bdb` with lighter links kept separate. This does not treat decorative separators as control boundaries or certify native accessibility. Full-screen visual/button review is recorded in [verification.md](verification.md).
