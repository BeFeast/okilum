# Ubuntu color-emoji fixture

`noto-color-emoji-cbdt.ttf` is a test-only subset of Ubuntu 26.04's
`fonts-noto-color-emoji` 2.051-1build1. The original installed file has SHA-256
`9fd0a3d0ce84d77e3185dfbae77bd1abf3926aa49a032e354d076c4f17151f10`.
The subset retains the real CBDT/CBLC bitmap tables and the GSUB rules needed by
the selected emoji. It deliberately contains no Latin `m` glyph, as in the stock
font. Original glyph IDs are retained: GPUI 0.3.3 skips glyph ID 3 as a variation-selector workaround, so renumbering a real emoji to that ID would make the fixture misleading. It is not embedded in production Okilum.

Copyright Google Inc. and the Noto contributors; SIL Open Font License 1.1.
The Ubuntu package's complete copyright/license notice is preserved alongside it.

Regenerate with fontTools 4.68.0:

```sh
pyftsubset /usr/share/fonts/truetype/noto/NotoColorEmoji.ttf --unicodes=U+0020,U+2705,U+274C,U+1F415,U+1F9E0,U+1F469,U+1F4BB,U+200D,U+FE0F --layout-features='*' --name-IDs='*' --name-legacy --name-languages='*' --glyph-names --notdef-glyph --notdef-outline --retain-gids --output-file=crates/okilum-shell/tests/fixtures/fonts/noto-color-emoji-cbdt.ttf
```
