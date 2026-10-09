# Sync supervisor: startup and lifecycle contract

Design slice for #1013, before any supervisor code (the shape is decided in
[the shape record](sync-supervisor-shape.md): a separate executable on macOS and
Windows). It fixes what the supervisor reads, what it refuses, how it runs and when it
exits, using only mechanisms that already exist (the journal store, the generation hint,
the transports, the owned trees). Linux has no supervisor.

## Inputs

- **Windows:** the task definition already passes exactly `--instance <uuid> --state
  "<dir>"`. **macOS:** the static launchd plist (`BundleProgram`, no arguments) passes
  nothing; the supervisor uses the owner's `~/Library/Application Support/Okilum/Sync`.
- The argv is closed: only those two optional options, any other argument is an error.
  `--instance`, when given, must equal the journal's. The environment is ignored, and
  nothing secret (no REST key, no credential) is ever in argv or environment.
- The state directory is opened through the store (owner-only: mode 0700 on macOS, the
  protected DACL on Windows) and must be the directory the journal's
  `Binding.state_directory` names.

## What it reads and refuses

1. The journal (`sidecar.json`, current v2 schema) through the store, never written by
   the supervisor. Absent, legacy, corrupt or any other unknown state: refuse to start.
2. `Binding.supervisor` must be this process's own executable (both resolved). A copy of
   the binary somewhere else, or a binary the binding does not name, refuses.
3. Lifecycle intent must be `Enabled`. Otherwise there is nothing to supervise: exit 0,
   which launchd's `KeepAlive/SuccessfulExit=false` does not relaunch.
4. **Runtime selection.** The supervisor launches only the runtime recorded in
   `<state>/runtime.json` (`{schema, version, digest, location}`, strict like the hint,
   written by the payload layer under the instance lock as the durable result of
   `update::Host::select`). `location` must be an absolute plain path under
   `<state>/runtime/`; the file's SHA-256 must equal `digest`, checked immediately before
   spawn while holding the file open without write or delete sharing where the OS allows
   it (Windows), and as close to the spawn as the OS allows elsewhere. A missing or
   mismatched selection refuses to start; it never falls back to another binary.
5. Config and data directories are `<state>/config` and `<state>/data`, separate and
   private (the existing `Launch` rules); the fixed Syncthing argv is the existing one.

## Run

Order matters, each step failing closed:

1. Create the transport endpoint for a fresh random generation (named pipe on Windows,
   socket in the state directory on macOS); a collision aborts.
2. Spawn the owned tree (`JobChild` / `ProcessGroupTree`) with the fixed argv and a
   cleared environment.
3. Publish the generation hint with the supervisor's own start time.
4. Serve exchanges one connection at a time: `begin_exchange(deadline)`, then
   `Server::serve_one` over the verified transport. The peer check on macOS requires the
   app's code requirement (identifier and Team ID from the release build configuration);
   on Windows the captured client process, as in `windows_transport`.
5. An authorized Stop that returns `Stopped` **ends the supervisor**: tree stopped,
   hint cleared, endpoint removed, exit 0. Start is the platform's (launchd, Task
   Scheduler), which is what `Platform::start` already means. A Stop that returns
   `Stopping` leaves the supervisor serving so the controller can retry.
6. Status reports the tree's status. Nothing else is served.

## Failure and restart

- If the runtime exits unexpectedly, the supervisor flushes the tree, clears the hint and
  exits non-zero. The single restart policy is the OS's (`KeepAlive` with its throttle,
  at most three task restarts); the supervisor has no restart loop. The app sees a
  missing hint or a refused connection and shows "Sync stopped" with the reason.
- If the supervisor itself dies, the Job Object (kill-on-close) or `ProcessGroupTree`'s
  `Drop` takes the tree with it; whatever escaped is exactly what `Stopped` already
  refuses to certify.
- The app quitting does not stop the supervisor: it is a login agent that outlives
  Reader, and only an authorized Stop ends it.

## Left to the implementation PRs, to be settled by measurement

- Whether the runtime is launched with `STMONITORED=1` (the compatibility fixture uses it
  because `--no-restart` alone leaves a monitor process); the tree owns both cases.
- A bounded, credential-free diagnostics file in the state directory.
- The longest supported macOS state-directory path for the socket (checked and refused at
  preparation, not at Stop time); to be measured on a real Mac with a long user name.
- The signature policy values and the development variant (compile-time `cfg` and a
  pinned digest), in the signing PR, taken from the release build configuration.
