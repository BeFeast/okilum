# Dev builds

A dev build is a branch or commit built on the machine that will run it, when a
fix has to be seen now (#2589). It replaces the installed Okilum: same app, same
state, same Sync. It gets a build number one above every published build, so the
updater never puts an older beta over it, and the next newer beta replaces it.
It is not a release: no notarization, upload, feed entry or provenance record.
Windows comes later.

## Mac: `okilum-sprint.sh <branch>`

Run it in Terminal on an Apple Silicon Mac, from the one Okilum checkout
(`~/Developer/personal/okilum`, origin = Forgejo SSH; the Mac cannot reach
`https://git.oklabs.uk`):

```bash
cd ~/Developer/personal/okilum && git fetch origin main && bash <(git show origin/main:scripts/dev/okilum-sprint.sh) <branch>
```

What it does:

1. Fetches the branch in that checkout and builds in its git worktree
   `~/Developer/worktrees/okilum/sprint/src`, with the warm Cargo target in
   `…/sprint/target` (`OKILUM_CHECKOUT` and `OKILUM_SPRINT_HOME` move them). No
   separate clone: an old `~/Developer/personal/okilum-sprint` clone is removed
   after its cache is moved. Local changes in the worktree stop the script.
2. **Build number:** one above the newest build in
   `https://updates.befeast.com/okilum/appcast.xml`. Without the feed, set
   `OKILUM_SPRINT_BUILD=<number above the newest beta>`.
3. Builds the `sprint` profile, adds the Sync helper and the pinned Syncthing
   runtime, and signs with the local Developer ID (`OKILUM_SIGNING_IDENTITY`).
   It does not notarize.
4. Closes Okilum and replaces `/Applications/Okilum.app`, the same way as the
   Linux delivery below (Oleg 14.11). Okilum is closed only after 60 s without
   keyboard or mouse input (`HIDIdleTime`, waits up to 2 min), and the idle
   time is read again before each step and right before the replacement.
   First Quit, then up to 60 s for Okilum to quit by itself: «User canceled.
   (-128)» is not a refusal, because Okilum cancels the first Quit while its
   updater/editor guard runs and then quits by itself. Then SIGTERM, which
   saves like Quit. Signals go only to the processes found at the start (pid,
   start time, exact `/Applications/Okilum.app` executable, your uid),
   checked again right before the signal. **No SIGKILL:** if Okilum is still
   open 30 s after SIGTERM, it keeps running. If the user is active or Okilum
   stays open, the build is kept and the Sprint exits 75 with `Sprint DEFERRED`
   and no report; the next run retries. If the user comes back after Okilum
   closed, the installed Okilum is reopened unchanged. The previous app is
   kept in `…/sprint/staging.noindex/previous-Okilum.app`. If the new app fails
   before launch, the previous one is put back. The old bundle's Sync helper
   (`okilum-sync-supervisor`, a launchd agent that outlives the app) gets
   SIGTERM after the replacement, never SIGKILL, and `launchctl kickstart`
   starts it from the new bundle; a failed kickstart is logged as a warning
   with its exit code.
5. **One registered app:** the staged and previous bundles live in
   `staging.noindex`, which Spotlight does not index, and are unregistered from
   LaunchServices. Any other registered `com.befeast.okilum` bundle (copies left
   by older versions of this script, a mounted DMG) is unregistered as well, so
   SMAppService and the Sync helper resolve to `/Applications/Okilum.app`.
   Unregistering deletes nothing.
6. **Lock:** `build.lock/owner` records the pid and start time. If that process
   is gone (crash, closed Terminal, reboot), the next run removes the lock. A
   lock without an owner, left by an older version of the script, is removed
   once it is 2 hours old. The same check applies to the old clone's lock.

Output goes to `…/sprint/stages.log`, compiler output to `build.log`. On failure
the existing report flow runs: diagnostics are filed through the feedback
endpoint and kept in `~/Library/Logs/OkilumSprint/last-sprint-report.txt`.

A failed build exits non-zero and its last line is `Sprint FAILED at stage
<stage> (exit N)`. The script already writes `stages.log` itself, so there is no
need to pipe it. If you do pipe it into `tee`, the shell reports `tee`'s status
(0), not the build's (13.11: a failed assemble-sign looked like exit 0). Print
the build's own status; macOS Terminal runs zsh, which numbers `pipestatus` from 1:

```bash
cd ~/Developer/personal/okilum && git fetch origin main && bash <(git show origin/main:scripts/dev/okilum-sprint.sh) <branch> 2>&1 | tee ~/sprint-run.log; echo "Sprint exit: ${pipestatus[1]}"
```

In bash the same value is `${PIPESTATUS[0]}`.

The script from main also builds branches older than
`okilum-macos-bundle-tools` (#2880): there it uses the old `bundled_plist` and
`macos_app_icon` examples, which produce the same bytes.

Status: the 05.11 changes (computed build number, `.noindex` staging,
LaunchServices cleanup, stale lock) are covered by `scripts/ci/test_dev_builds.py`
on Linux. They have not run on a Mac yet. No agent runs this script over ssh.

## Linux: `okilum-dev-linux.sh <branch|commit>`

Run it on the Linux machine itself, from your Okilum checkout:

```bash
cd ~/path/to/okilum && git fetch origin main && bash <(git show origin/main:scripts/dev/okilum-dev-linux.sh) <branch-or-commit>
```

1. **One repository:** it fetches in the checkout it is run from (or
   `OKILUM_CHECKOUT`) and builds in a git worktree of it,
   `~/.cache/okilum-dev/src`. Only without any checkout does it clone one into
   `~/.cache/okilum-dev/repo` (`OKILUM_DEV_REPO`, default
   `https://git.oklabs.uk/BeFeast/okilum.git`). The checkout's origin must be
   BeFeast/okilum.
2. A branch, or a commit on any origin branch, is checked out in the worktree.
   The warm Cargo target is `~/.cache/okilum-dev/target`; `OKILUM_DEV_HOME` moves
   worktree, target and logs. Cargo never prunes it, so once it passes
   `OKILUM_DEV_TARGET_CAP_GIB` (default 30) GiB the next build starts it fresh.
   Inside a `build-slot` run the slot's target is used instead and the slot
   bounds it. Local changes in the worktree stop the script,
   and so does a ref whose `scripts/appimage/build.sh` predates `OKILUM_LOCAL_DEV`
   (merge main into it).
3. **Build number:** one above the newest build in
   `https://updates.befeast.com/okilum/linux/{beta,stable}/latest.json`, or
   `OKILUM_DEV_BUILD`.
4. Builds with `OKILUM_LOCAL_DEV=1 scripts/appimage/build.sh`: the same AppImage
   steps as CI, minus the Ubuntu 22.04 base check, linuxdeploy stripping and the
   CI provenance record. The profile is `release` (`OKILUM_DEV_PROFILE` changes it).
   The AppImage carries this machine's libraries, so it is for this machine,
   not for testers.
5. **Install:** `~/.local/share/okilum/Okilum.AppImage` is replaced by a rename
   while Okilum's own install and update locks are held. An update that the
   running app has staged or is applying is never overwritten. The previous
   file stays as `Okilum.AppImage.previous`. To go back:
   `mv -f ~/.local/share/okilum/Okilum.AppImage.previous ~/.local/share/okilum/Okilum.AppImage`.
6. **Restart:** Okilum windows are found by `/proc/<pid>/exe` (an `okilum`
   executable) holding the single-instance lock, never by command line, so
   `okilum sync serve` is left alone. The window gets SIGTERM (Okilum saves and
   exits) and up to 30 s to quit; it is never killed. The new build then starts
   with the old window's environment, minus the AppImage runtime variables, so
   this also works over ssh. `--no-restart` skips this step.

Exit status: `0` installed (and restarted if Okilum was running), `1` failed
(nothing installed; a window that was quit is started again), `3` installed, but
Okilum did not quit and keeps the old build until it is reopened. Logs: the last
20 runs in `~/.cache/okilum-dev/logs`.

Needs Rust, git, curl, python3, gpg and gpgv, flock (util-linux), readelf
(binutils) and the system libraries gpui builds against (Wayland, xkbcommon,
fontconfig, freetype, xcb, Vulkan).
