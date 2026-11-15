# Value-copyable shaped glyphs (#2803)

The pinned `gpui-pre` 0.3.3 `ShapedGlyph` contains only value types:
`GlyphId`, `Point<Pixels>`, a byte index and an emoji flag. All fields are
already `Copy`, but the enclosing glyph derives only `Clone`. Consequently
`Vec<ShapedGlyph>::clone` uses element cloning instead of the standard library's
bulk-copy specialization. The returned layout of a multi-MB source row copies
millions of glyphs per edit.

This patch adds `Copy` to that one derive. It changes no fields, coordinates,
shaping, ownership, allocation lifetime, cache admission, memory cap or public
method. The compiler rejects the derive if a future glyph gains an owned field.
The application keeps its ordinary `Vec` clone and native geometry checks.

On one approved Linux build slot, two alternating release/mimalloc rounds
measured 27 edits in the same 3.56 MB row: median layout cost changed from
23.168 / 23.552 ms to 17.261 / 17.170 ms. Maxima were 81.091 / 77.057 ms before
and 39.230 / 40.549 ms after. These headless timings retain the outliers and do
not establish installed key-to-screen acceptance or a 16 ms frame budget.

The archive, patch and resulting tree are checksum-pinned in
`scripts/gpui-pre/manifest.json`. The setup wrapper reuses the existing verified
archive extraction and patch mechanism. An upgrade must reassess the fields and
refresh the three digests; a missing/changed upstream derive fails patch
application rather than silently changing behavior. This is a separate scoped
GPUI bug-fix patch under the authorized vendored GPUI work, not a new layout API.

The source owner is [zed-industries/zed](https://github.com/zed-industries/zed),
`crates/gpui/src/text_system/line_layout.rs`. The snapshot's
[package metadata](https://docs.rs/crate/gpui-pre/0.3.3/source/Cargo.toml.orig)
identifies Zed revision `5b055fa789a8b8d38ac951a6e0cde272f66b4495`.
[crates.io owners](https://crates.io/api/v1/crates/gpui-pre/owners) identifies
Jason Lee (`huacnlee`) as the snapshot publisher; publishing the snapshot does
not make it the owning source repository. This derives a general value property
with no Okilum hook and is suitable for upstream submission. The plan is to
check current upstream for the same type, submit the minimal derive change with
the clone-cost measurements if still applicable, and remove the downstream diff
after a snapshot includes it. Opening an upstream issue or PR requires Oleg's
separate approval; none has been opened.
