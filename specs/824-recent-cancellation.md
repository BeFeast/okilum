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
