# Sync supervisor: separate executable or a mode of the app binary

Decision record for #1013 (follow-up of #588). One page, decided before any supervisor code.

## Decision

A separate small executable, `okilum-sync-supervisor`, for macOS and Windows. It is a
new binary crate that depends on `okilum-sync-controller` and not on `okilum-shell`, so
it carries no GPUI, Reader or editor code. Linux has no supervisor: the per-instance
systemd user unit (`okilum-syncthing-{id}.service`) runs Syncthing directly and systemd
owns the process tree through its cgroup, which is stronger than anything we could build.

## Why not a mode of the app binary

1. **Windows updates.** Velopack replaces the `current` folder on update. The existing
   task design already requires the supervisor executable to be staged *outside*
   `current` (docs/sync-sidecar-lifecycle.md). A mode of the app binary would mean
   staging a full copy of the app there and keeping it in step with every release, and a
   background process running from `current` would lock files and block update and
   uninstall (#974). A small dedicated executable is cheap to stage, verify and pin by
   digest.
2. **Login-time footprint.** The supervisor is started at login by launchd or Task
   Scheduler and must stay up while Reader is closed. Running the GUI binary there loads
   a large image and risks any GUI initialisation on a background path. The supervisor
   needs the process tree, the generation hint, the journal store and one socket or pipe.
3. **Identity and attack surface.** The peer check pins a code requirement and an exact
   executable path (#963, #948). A distinct executable gives the helper its own code
   identity (macOS identifier from the same build configuration as the app's, with a
   `.sync` suffix; it matches the launchd label `com.befeast.okilum.sync`) and an image
   that contains no document parsing, no UI and no network client beyond what Sync needs.
   Mutual authentication then names two different programs instead of one.
4. **Existing adapter contract.** `bundled_plist` already assumes a helper under
   `Contents/MacOS` addressed by `BundleProgram`, with no `ProgramArguments`; the
   Windows task has one Exec action and a pinned path. A mode switch would need new
   arguments in both and a second code path in `main` that must never reach GUI start-up.

## What it costs, accepted

A second artifact to build, sign and stage.

- **macOS:** the helper lives at `Contents/MacOS/okilum-sync-supervisor` in the same
  bundle. `sign-bundle.sh` signs inside-out from an explicit list, so it gains one line
  that signs the helper (hardened runtime, with `--identifier` set to the helper's code
  identifier) before the outer bundle; notarisation covers it with the app. Sparkle
  replaces the bundle as a whole, so helper and app can never be out of step.
- **Windows:** the Velopack package carries it; the payload layer stages a versioned copy
  outside `current`, verifies its signature and digest, and the task action points there.
  A new app release stages a new version and the old one is stopped through the normal
  Stop path first. The IPC `VERSION` check refuses a mismatched pair either way.
- **Console window:** it is built for the Windows GUI subsystem so a login-triggered task
  never flashes a console.

## Consequences for the work

Implementation PRs, one at a time, each tested on its own: (1) the binary crate with the
serve loop over the existing transports, writing and clearing the hint, exiting with its
tree; (2) packaging and staging on macOS and Windows; (3) the discovery glue in the app;
(4) `supervisor_scope` / `stop_supervisor` in the two adapters; (5) signature policy
values taken from the release build configuration (macOS Team ID and bundle id from the
release-signing variables; Windows subject and issuer chain after the Certum certificate
exists; until then a development `cfg` with a pinned digest). Native acceptance per OS
closes the issue: start, app restart reconnects, supervisor crash is shown and restarted,
quit policy, uninstall removes the units.

## What would change this

If the Windows payload layer cannot stage a second executable outside `current`, or if
notarising a second nested helper proves to break Sparkle's whole-bundle update, the
mode-of-the-app alternative returns, with the costs above. Neither is expected: nested
helpers in `Contents/MacOS` are the platform's normal shape.
