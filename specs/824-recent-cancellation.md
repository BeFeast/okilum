# Cancel an in-progress Recent gesture (#824)

Escape must dismiss the Recent overlay while Control (and optionally Shift) is
still held. Capture the physical Escape key only while the overlay is open;
leave the existing Escape actions untouched otherwise. A later Control release
must not navigate or reorder Recent.

Cancel the gesture across window activation boundaries. Re-entry must also clear
any abandoned gesture before the compositor delivers its current modifiers;
release while inactive must never commit. Preserve ordinary Control-release
navigation and the existing editor conflict guard.

Validate from Reader and Source focus, with Control held for Escape, and with a
window blur/re-entry followed by a delayed modifier release. Include positive
controls that the overlay opened and normal release still navigates. Capture
before/after in Linux light and dark; Wayland workspace switching is rechecked
by native QA on the published beta.

## Wayland ordering regression

A modifier-clear event alone does not prove a physical Control release: focus
transfer can report cleared modifiers while the window still appears active.
Queue the release for a 100 ms settling interval. Activation changes, Escape,
re-pressing Control, or starting/stepping another gesture invalidate its generation.
At the commit boundary, recheck the generation, active window and Control state.
No save/navigation happens before that boundary. The short interval preserves
normal release behavior while letting the platform's queued focus events cancel.

The native regression delivers modifier-clear first, checks that the original
note is still open, then deactivates and reactivates the window before settling.
Existing tests retain normal-release, Source/conflict, Escape and deactivation-
before-release controls. Real Hyprland workspace validation is delegated to QA
on the published beta; no gpui core patch or local heavy build is introduced.
