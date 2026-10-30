# Live Stable baseline admission

Apply canonical.patch to exact canonical bcb0106, together with live_baseline.py
and test_live_baseline.py copied to autosave-gate/. No alternate native driver.
Fresh Stable maxFull selects a reviewed retained producer pin; package digest and
retained Setup digest must match. Unknown builds fail closed. Native step01 still
requires Authenticode Valid before NOARGS Setup, and existing identity/preservation
checks remain unchanged. Target remains the fresh real beta feed selection.

19179 package/source/Setup pins: qa-evidence/windows-19179-signing/release.json,
asset-checks.json and signed-verification.json. Compiled18194 versus package19179
mapping is retained in qa-evidence/1698/pr-native-r4 and the reviewed Stable mode.
17312 mapping retains the previous default pins. Tests use synthetic Setup bytes
for admission controls, not an Authenticode claim. Native W2 requires merge,
Manager authority and fresh CI shared-slot admission; first blocker STOP.
