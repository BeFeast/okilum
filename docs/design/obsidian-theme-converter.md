# Obsidian theme converter (#3157)

`scripts/themes/obsidian-theme-convert.py` turns an Obsidian community theme's
CSS into an `okilum-theme/v1` file and a Markdown report. It runs offline,
never in the app: what ships is the plain theme JSON in
`crates/okilum-shell/assets/themes/`, so a converted theme costs nothing at runtime.

```bash
scripts/themes/obsidian-theme-convert.py theme.css --id obsidianite --name Obsidianite \
    --credit "github.com/bennyxguo/Obsidian-Obsidianite, MIT" \
    --out obsidianite.json --report obsidianite-report.md
```

It exits 1 if a contrast gate still fails after adjustment. `--high-contrast`
raises reading text to AAA (7:1). The output is a starting point: a person
reviews it, tunes it and adds it to `ThemeId` like any other built-in theme.

## Variables

Custom properties are read from `:root`/`html`, `body` and `.theme-light` /
`.theme-dark` (also `body.theme-dark`), in Obsidian's precedence order:
`:root` < `body` < theme class, and later beats earlier within one level.
Underneath sits a short table of Obsidian 1.x app defaults (`--color-base-*`,
`--color-accent` from `--accent-h/s/l`, `--color-red` … with their `-rgb`
triplets), so a theme's own variables resolve the way Obsidian resolves them.
A token that would come only from those defaults (neutral greys, Obsidian's
purple accent) is derived from the theme's own colours instead; status and
callout hues may keep Obsidian's values.

Values resolve through `var()` chains with fallbacks, RGB triplets
(`rgba(var(--x-rgb), .5)`, including a hex value placed in an `-rgb`
variable), `rgb()`/`hsl()` in comma and space syntax with `calc()`, `color-mix()`
(srgb, oklab) and named colours. Translucent colours are composited over the
canvas, except `highlight` and `scrim`.

Blocks with extra classes (Style Settings variants such as
`.theme-dark.minimal-dark-black`), descendant selectors and blocks inside
`@media`/`@supports` are not scopes; the report lists them.

## Dialects

| Token | pre-1.0 source | 1.0+ source |
| --- | --- | --- |
| `accent` | `--text-accent`, `--interactive-accent` | `--color-accent`, `--interactive-accent`, `--text-accent` |
| `link` | `--text-a`, `--text-accent` | `--link-color`, `--text-accent` |
| `heading`, `heading1`…`heading6` | `--text-title-h1`…`h6` | `--h1-color`…`--h6-color` |

Both dialects share the rest: `--background-primary` (canvas, surface),
`--background-secondary` (raised surface, sidebar), `--text-normal`,
`--text-muted`, `--text-faint`, `--background-modifier-border`,
`--text-selection`, `--background-modifier-hover`, `--code-background` (or
pre-1.0 `--pre-code`), `--text-highlight-bg`, `--link-unresolved-color`,
`--callout-question`, `--callout-important`, `--background-modifier-cover`
and `--color-green/orange/red/blue` for status. The report names the variable
chain behind every token. The dialect is the one with more of its marker
variables present.

Tokens the theme does not name are derived from those it does (tile from
accent over canvas, find colours from highlight, and so on); the report says how.

## Headings and links (#3147)

A heading must be coloured and at least 25° in hue from links:

- H1 in the text colour (most 1.0+ themes): links stay as they are. The
  heading becomes the accent if that is far enough from the links, otherwise the
  theme's own colour nearest in hue to the accent (both modes take the same variable).
- Coloured H1 too close to the links (Obsidianite's cyan H1 and `--text-a`): the
  heading stays, and the links take the theme colour nearest to their own hue that
  is far enough away.
- An unresolved link that looks like a link or a heading takes `warning`.

## Derived mode

A theme that declares only one mode gets the other one derived. Surfaces
mirror their lightness around a new canvas (0.975 light, 0.09 dark) and keep
their hue, with saturation capped. Text and accents keep hue and saturation and
take the lightness that gives them the same contrast to the new canvas as they
had in the native mode, limited to 4.6:1–5.5:1 for accents. Decoration colours
follow the token they match.

## Contrast

The converter applies the gates of `brand.rs`
(`every_theme_meets_wcag_contrast_for_text_muted_and_links`,
`issue3147_every_theme_colours_headings_apart_from_text_and_links`,
`code2810_scrollbar_mark_roles_have_contrast_in_every_theme`) plus every
heading level and every syntax colour on `codeBg`. A failing foreground moves in
lightness, hue kept; for `text on X` the background X moves; `onAccent` and
`onStatus` take the first of the theme's own value, canvas, white, black and text
that passes. Every change is in the report's "Adjusted" table, and every gate's ratio is in
its "Contrast" table.

## Decoration slots and syntax

Recognised patterns fill per-mode `decorations` and `syntax` objects next to the
colour tokens. The app reads `syntax` (#3156, schema in `docs/design/reader.md`
§Code syntax palette): keys are tree-sitter capture names, so the converter's
`builtin` slot is written as `function` when the theme set no `function`
colour, and dropped otherwise. The app reads decorations from a top-level
`decorations.light` / `decorations.dark` object (#3154, schema in
`docs/design/reader.md` §Decoration slots); the converter's per-mode
`decorations` below is not read until it follows that schema.
Every gradient slot takes two stops; more stops are reduced to the first and
the last distinct colour (`toAt` keeps where the last colour starts).

| Slot | Recognised CSS |
| --- | --- |
| `headingRule` `{levels, from, to, toAt?}` | `border-image(-source): linear-gradient(...)` on `h2`…`h6`, `.HyperMD-header-N`, `.cm-header-N` |
| `hr` `{edge, middle}` | gradient `border-image` on `hr` |
| `blockquote` `{angle, from, to, border, bars: {top, bottom}}` | gradient `background` on `blockquote` / `.HyperMD-quote`, its `::before`/`::after` bars (with `width`), `border-left` colour |
| `linkMarker` `{internal, external, heightPx}` | gradient `background-image` on `.internal-link` / `.external-link` / `.cm-link`, `background-size` height |
| `boldGradient` `{angle, from, to}` | `background-clip: text` with a gradient on `strong` / `.cm-strong`; both ends kept at reading contrast |
| `syntax` (comment, string, number, keyword, function, variable, type, builtin → function, operator, property, tag, attribute) | `color` on `.cm-*` (CodeMirror) and `.token.*` (Prism) classes |

## Report

The report starts with a summary (dialect, derived mode, gate count,
adjustments) and then lists, per mode, each token's value and source chain,
the adjustments, every contrast ratio, the decoration and syntax results, and
**everything not carried over**: each at-rule (`@font-face`, `@keyframes`,
`@import`), each variant or conditional variable block, and each declaration
by line, selector and property, grouped by reason (fonts and metrics, motion,
`@media` rules, styling with no Okilum token or slot). Colour declarations that
only point at a variable the converter mapped (`color: var(--text-title-h1)`)
arrive through that token and are not listed. The report ends with the theme
variables that no token read.
