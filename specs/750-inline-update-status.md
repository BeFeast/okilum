# Inline manual update checks (#750)

Manual Check for updates from Settings, About and the macOS app menu opens
Settings → Updates. Checking, no-update, compatibility and retryable check errors
appear in that section. No progress or “You're up to date” modal is created.

The public Sparkle 2.10.0 `SPUUserDriver` adapter forwards permission, new-update,
download, installation and relaunch interactions to `SPUStandardUserDriver`.
Only a manual check's terminal not-found/error callback is intercepted, and its
acknowledgement is invoked exactly once. After update-found the standard driver
owns errors again. A currently presented standard interaction is brought forward,
never reclassified as a new manual check. The updater, delegate and both drivers
are retained for the process lifetime. All bridge access runs on the main thread,
as required by Sparkle. FFI exposes integer state only, with no borrowed strings.

Compatibility reasons stay distinct: latest release, newer installed version,
newer/older macOS requirement, ARM64 hardware requirement, and unknown availability.
No raw network errors or file paths appear in the settings status. The refresh
glyph is disabled while checking and its tooltip becomes Retry after failure.
One short-lived GPUI observer refreshes windows only on status changes and exits
when the check/standard interaction finishes. No new preference storage is added.
Linux system-managed and Windows updater behavior are unchanged.

## Native verification

On an authorized macOS development/QA host with the pinned Sparkle framework
prepared by `python3 scripts/updater/sparkle.py prepare`:

```sh
mkdir -p "$HOME/.cache/tessera-qa/750"
xcrun clang -fobjc-arc -fmodules -F vendor/sparkle \
  -framework AppKit -framework Sparkle \
  -Wl,-rpath,"$(pwd)/vendor/sparkle" \
  crates/tessera-shell/tests/updater_bridge_probe.m \
  -o "$HOME/.cache/tessera-qa/750/updater-bridge-probe"
"$HOME/.cache/tessera-qa/750/updater-bridge-probe"
```

This probe asserts real adapter callbacks, exact acknowledgements, and forwarding
positive controls without contacting a feed or opening a stock dialog. It does
not replace packaged-app QA: test current-version, available-update, offline
failure→Retry, repeat clicks, app menu and About. Verify standard download/install
errors still appear in their own workflow and quitting/reopening Settings is safe.
Native compilation and runtime QA are required before merge; Linux tests cannot
validate the Objective-C bridge.

Linux visual-only fixture (explicit non-release features):
`--features settings-ui-harness,updater-ui-harness`, `TESSERA_DEBUG_UPDATER_UI=sparkle`
and `TESSERA_UPDATE_CHECK_FIXTURE=1` (checking), `2` (up to date), `4` (macOS
compatibility), `8` (failure). Omit the latter for idle. These fixtures never start
an updater, change updater preferences, contact a feed, or claim macOS behavior.
