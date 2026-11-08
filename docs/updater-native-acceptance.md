# Native beta update acceptance

Use this procedure for the installed Linux AppImage, macOS app and Windows
installer build. Hosted tests of the scheduler, signed Sparkle fixtures and
package admission are necessary controls; they do not establish that a tester
received an update. See [update behavior](update-modes.md).

## Establish the installed baseline

Use each approved native test host and its existing transport. Respect its
installation, signing and GUI ownership rules. Do not build on a test host when
only installation and execution are authorized. Work in a synthetic vault and
identify its exact process/window before input or capture. Preserve other apps,
windows and user data.

Record the installed version, compiled source, installation path, channel and
update mode. Keep a canonical synthetic note with a recorded SHA-256, and record
its open root/note/window mode. On an existing profile, inspect only the test
window and scoped updater diagnostics. Capture About and the test window rather
than unrelated content.

## Exercise a real update

1. Record the public Beta offer and its publication time. Resolve the Linux
   signed descriptor, macOS appcast or Windows versioned release descriptor to
   its exact build/source. Confirm the source contains the intended fix.
2. Leave Background mode running without manual Check. Record when the newer
   offer becomes Ready. Separately return to the test window after the shared
   automatic-check throttle has elapsed. Distinguish interval discovery from a
   focus-triggered check; opening About or pressing Check is not evidence for an
   unattended interval. The configured timer is 28–30 minutes.
3. Capture the single update notice and its Restart action. When another real
   beta is published while Ready, preserve evidence of the older package/offer
   and repeat discovery. The newest Ready replaces the same notice, including
   in a window that lost focus. There must be no stacked Ready and unavailable
   notices.
4. Click Restart now once. Record consent time and the scoped `update_restart`
   attempt/outcomes. Follow freshness verification and any superseding download
   through relaunch. Do not supply a second click to hide a lost continuation,
   replace the installed app manually, delete state or apply recovery steps and
   then call the normal update successful. Report a guard or failure with the
   actual reason; protect unsaved data.
5. Resolve the new process again. Confirm the compiled build/source, Beta
   channel, original root/note/window mode and unchanged canonical-note hash.
   About and What's new must name the installed build and its channel-specific
   notes. Record whether manual recovery was needed.

Installing a fixed beta through the old updater establishes delivery. Repeat
the transition **from the fixed beta to a later beta** to exercise the shipped
implementation itself. Keep the earlier negative control distinct from this
acceptance result: an old Ready can install a superseded build successfully and
only discover the current beta after relaunch.

## Platform evidence

- **Linux:** compare the managed AppImage hash with the authenticated descriptor
  and retain the helper's startup-confirmed/transaction-committed receipt. An
  unarmed stage is not an installed build; a successful process alone is not a
  committed update. Preserve the launcher target and channel preference.
- **macOS:** verify the installed bundle/executable, identify the synthetic
  window again after relaunch and retain the attempt's native lifecycle events.
  An empty termination-retry callback can occur while asynchronous quit is
  finishing; it must remain waiting rather than produce a false refusal.
  A real refusal reason still fails the attempt. Never infer the actor or consent
  from a version change alone.
- **Windows:** verify the actual installed executable's build information and
  package version, not only the downloaded package. A Velopack delta reconstruction
  can have different ZIP bytes from the published Full archive. Its admission
  receipt must bind local bytes, channel and version to the authenticated Full
  offer; a digest mismatch by itself does not establish corrupt reconstruction.
  Test through the normal tester profile as well as any isolated fixture profile.

Keep versioned descriptors, source SHAs, timestamps, screenshots, note hashes,
restart records and a per-platform result together. Report verified, merged,
published and installed milestones separately. Close delivery work only after
the required native transitions and user-visible behavior pass on all three OSes;
publication and merge alone are not completion.
