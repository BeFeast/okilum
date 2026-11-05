# Sync settings and pairing

Issue #2615 implements Oleg's selected layout A from the November 5, 2026
Sync settings study: one actionable status, Computers, Vaults, and collapsed
Advanced controls. Add computer opens a guided modal with an explicit local
computer name, source invitation, receiving-computer input, approval wait,
authorized vault selection, and first-transfer observations. Closing the modal
does not cancel pairing or a transfer; Cancel request is explicit.

Incoming approval requests appear in the Reader and Settings, including other
Settings sections, and trigger one desktop notification per request. Linux uses
notify-send; macOS uses an AppleScript notification with separate argv values.
Approval remains an explicit in-app action. Errors use plain language and an
action; the complete redacted cause chain remains in sync_action_failed and
behind Details. Invitation capabilities and keys remain redacted.

The existing authenticated Pending polling protocol and topology are unchanged.
A lost or timed-out Redeem reply leaves the durable request pending. Subsequent
polls reuse its exact request identity until approval, rejection, cancellation
or the invitation deadline; malformed or unauthenticated replies remain errors.
Ordinary catalog and transfer requests retain their existing socket bounds.

Computer connectivity, last seen, shared folder count, and local vault transfer
observations come from read-only Syncthing REST calls on the background executor.
Unavailable observations remain unknown. The current pairing protocol does not
advertise the peer's OS, so these peers say "OS not reported" rather than infer
an OS from a computer's name. No topology or protocol migration is introduced.
