# Window scale without a screen on macOS (#3244)

**Status: proposed; needs Oleg's OK before merge** (core `gpui-pre-*` patch rule
in `AGENTS.md`).

`gpui-pre-macos` 0.3.3 `get_scale_factor` returned `2.0` whenever
`[NSWindow screen]` was nil, for example when a window is placed outside
every screen before it is moved. GPUI's initial content-rect placement is
wrong for secondary screens; that is why Okilum's `window_state` restore sets
the outer frame itself. On a Mac whose displays are all 1x (the reporter's:
two 1080p panels run at 1600×900, backing scale 1.0), that fallback is the
only way GPUI can render at 2×, and a 2× frame shown on a 1× display reads
as soft.

`scripts/patches/gpui-pre-macos/0004-window-scale-without-screen.diff` reads
the window's own `backingScaleFactor` instead. The existing zero guard
(zed-industries/zed#6412) is unchanged. It is pinned by
`scripts/gpui-pre-macos/manifest.json` and verified by
`scripts/gpui-pre-apple-setup.py`.

The app-side half needs no vendor change: a restored window is moved
(`setFrame`) while it is still being built, and native resize or backing
callbacks cannot reach an unregistered GPUI window (`try_borrow_mut` fails and
the error is only logged). `ui_zoom::init` re-reads the platform scale and
size (`Window::bounds_changed`) once the window is registered.

## Ownership and upstream plan

The [crates.io owners](https://crates.io/api/v1/crates/gpui-pre/owners) list
Jason Lee (`huacnlee`) as the snapshot publisher. The source owner is
[zed-industries/zed](https://github.com/zed-industries/zed) (`crates/gpui_macos`).
The change is generic (no Okilum hooks) and suitable for an upstream PR to
Zed's macOS window backend. Opening one needs Oleg's separate approval.
Rebase the patch on every snapshot upgrade.

## Verification

Linux CI compiles nothing here. The hosted macOS clippy job
(`gh-check.sh --macos-sync --dispatch`) compiles it. Native acceptance means
reproducing #3244 on the reporter's Mac before and after, with the
#3244 diagnostic fields (`window_on_screen`, `render_scale`,
`window_backing_scale`, `drawable_matches_layer`) in the feedback report.
