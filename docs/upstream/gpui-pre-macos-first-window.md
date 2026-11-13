# Cold first window on macOS (gpui-pre-macos / gpui-pre-apple 0.3.3)

Oleg approved two core GPUI patches on 2026-11-13 (#2825). They remove work from
the process's first `open_window`, which blocked the UI thread for about 1.7 s on
hosted `macos-15` and made the unchanged 2 s cold `data.xml` event-loop gate fail
in 3 of 5 observed runs (1311, 1993, 2462, 2485, 2549 ms).

## Where the time went (exact main, Time Profiler, main thread)

| phase inside the first `open_window` | wall ms | on-CPU ms |
|---|---:|---:|
| `NSWindow initWithContentRect` | ~175 | ~170 |
| `newLibraryWithSource` (runtime shader source) | ~810 | ~10 |
| eight `newRenderPipelineState` | ~50 | ~40 |
| `makeKeyAndOrderFront` | ~40 | ~12 |
| `Window::new` → `tab_bar_visible` → `[NSWindow tabGroup]` | ~590 | ~37 |
| Okilum view construction | ~10–31 | ~10 |

## Patches

1. `scripts/patches/gpui-pre-macos/0001-skip-tab-group-without-tabbing.diff`:
   `MacWindow::tab_bar_visible` returns `false` without sending `tabGroup` when the
   window has no tabbing identifier. GPUI already disables automatic window
   tabbing for such windows, so they cannot show a tab bar.
2. `scripts/patches/gpui-pre-apple/0001-shared-background-metal-pipelines.diff`
   and `scripts/patches/gpui-pre-macos/0002-prepare-metal-pipelines-at-startup.diff`:
   the shader library and eight pipeline states are built once per process for
   the first device on a background thread, started from `MacPlatform::new`, and
   shared by every renderer. The first draw that encodes a batch waits for them if
   the build is still running, so no frame is drawn without them; a failed build
   still panics there with the original message.

`scripts/gpui-pre-apple-setup.py` extracts the exact crates.io archives, applies
the patches in order and verifies archive, patch and tree digests and the list of
changed files, like `scripts/gpui-pre-windows-setup.py`.

## Upstream

The code owner is zed-industries/zed (`crates/gpui_macos/src/window.rs`,
`crates/gpui_macos/src/platform.rs`, `crates/gpui_apple/src/metal_renderer.rs`);
the `gpui-pre*` crates are snapshots published by Jason Lee (`huacnlee`). Each
patch file starts with its upstream commit message. Both are general (no Okilum
hooks). Opening the upstream PRs needs Oleg's separate approval.

## Rollback

Remove the two `[patch.crates-io]` lines from `Cargo.toml`, restore the registry
`source`/`checksum` lines for both packages in `Cargo.lock`, and drop the
`gpui-pre-apple-setup.py` call from `scripts/vendor-setup.sh`.
