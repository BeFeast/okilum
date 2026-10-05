# Native brand foundation

Issue [#101](https://git.oklabs.uk/BeFeast/tessera/issues/101) integrates approved
brand delivery B1.2.0 and GUI V2. This module supplies appearance and assets;
application routing, callbacks, editing and provider behavior belong to the shell.

## Sources and precedence

`crates/tessera-shell/assets/brand/manifest.json` pins every production asset,
its byte length and SHA256, and every input path/hash at its exact source commit.

- Brand: `7bf512a0041b51b90b88652aae12bef6ead17b83`, canonical B symbol and app
  icons from `design/brand/recommended/`; brand tokens remain version 1.1.0.
- GUI: `3ba23948adbabb4288bb3bc6fa0d3e4e074861e2`, interface aliases 2.0.0.
- GUI neutral surfaces, blue controls, focus and link aliases override brand
  surface aliases. Status colors and typography retain brand definitions.
- SVG bytes are copied unchanged. The shell displays the single-color canonical
  symbol because GPUI SVG uses an alpha mask; the packaged app icon preserves its
  full SVG colors. No exploration board, earlier mark or synthetic data is shipped.

## Native typography

Noto Sans and Cascadia Code are embedded in the executable at weights 400, 500, 600
and 700. Approved WOFF2 sources are instantiated at each weight, their Latin and
Cyrillic subsets merged into one native TrueType face, and native style names
assigned. This avoids competing subset faces with identical family names. The
manifest records the conversion and glyph coverage; original font notices remain
beside the assets. There is no OS font install or runtime download.

English and Russian are covered by the embedded fonts. Other scripts use platform
fallback. The approved sources are upright; italic can be synthesized. Register
fonts before constructing windows so an earlier font lookup cannot cache fallback.

Reproduce assets from the pinned design checkouts:

```sh
uv run --with fonttools==4.64.0 --with brotli==1.2.0 python scripts/brand-assets.py import \
  --brand-source /path/to/tessera-brand-identity \
  --gui-source /path/to/tessera-app-gui
python3 scripts/brand-assets.py verify
```

## Integration hooks

Add `mod brand;` beside the shell's other modules. Replace the toolkit `Assets`
provider with `brand::Assets`, which also delegates all toolkit icons. After toolkit
initialization, call `brand::load_fonts(cx)?` and `brand::apply_theme(cx)`. Call
`apply_theme` again immediately after every `Theme::sync_system_appearance` or
`Theme::change`, including the first window's appearance sync. It updates both the
legacy colors and resolved `ThemeTokens`, then projects typography to GPUI Base
and the Reader. Keep the toolkit's matching syntax highlight theme.

`palette(cx)` returns a Copy palette of semantic Hsla colors. `control(id,cx)`
creates a 36px native secondary button with 8px radius; existing `.primary()`,
`.ghost()`, `.disabled()`, `.loading()` and click callbacks remain usable.
`button(id,label,kind,cx)` also provides 38px primary and quiet/destructive actions.
Avoid forcing instance background/text styles over native state styles, which
would overwrite disabled/hover colors. `logo(size_px,cx)` displays the canonical B
symbol; size and placement belong to the application layout.

`SANS_FONT`, `MONO_FONT`, `CHROME_FONT_SIZE` (14), `READING_FONT_SIZE` (15) and
`MONO_FONT_SIZE` (13) provide coherent typography; the Reader can keep its explicit
reading size and heading hierarchy.

## Packaging

The macOS artifact lane renders the approved `app-icon-light.svg` through GPUI's
full-color SVG renderer in a build-only example. Its straight BGRA output is
converted to RGBA PNG iconset representations, then native `iconutil` creates
`Contents/Resources/Tessera.icns`. `CFBundleIconFile` declares it before codesign.
The build checks a native iconset round trip and records the source SVG and ICNS
hashes in the artifact receipt. No GUI startup or tool installation is needed.

```sh
python3 scripts/brand-assets.py package /new/package-stage
```

The helper verifies every production file and creates a new directory containing
`share/tessera/brand/` and `share/icons/hicolor/scalable/apps/tessera.svg`. This
stages files only; it does not modify any desktop, icon cache, fontconfig or OS
settings. Fonts and shell symbols already live inside the binary. Packaging keeps
the manifest and notices available outside it for inspection. Merge this stage
with the source-pinned binaries and launcher when assembling a release.

## Verification boundaries

The focused Rust regression flips light→dark→light and checks native button,
sidebar and focus tokens plus font families. `brand-assets.py verify` checks exact
inventory and hashes. These checks do not establish actual native font rendering,
visual acceptance, accessibility or installation; the integrated shell's native
review supplies that evidence separately.
