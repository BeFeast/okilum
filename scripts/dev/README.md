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
4. Quits Okilum and replaces `/Applications/Okilum.app`. The previous app is
   kept in `…/sprint/staging.noindex/previous-Okilum.app`. If the new app fails
   before launch, the previous one is put back.
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

## Linux: newest beta on every merge, `okilum-beta-deliver.py`

Oleg 14.11: every merge to main reaches his Linux desktop within 30 minutes,
with nobody in the loop, and Okilum is closed only after 60 s without input.
Nothing is compiled on the desktop: the script installs the hosted Linux beta
that CI publishes for every main push
(`https://updates.befeast.com/okilum/linux/beta/latest.json`). A systemd user
timer runs it 2.5 min after the previous run ends, in the graphical session.

One run:

1. **Feed:** reads the signed beta descriptor and checks it the way the in-app
   AppImage updater does (`crates/okilum-appimage/src/lib.rs`): gpgv against
   the pinned release key (fingerprint and key are copies of the crate's,
   `scripts/ci/test_beta_deliver.py` keeps them equal), every descriptor field,
   the payload signature, size and sha256, and the signed provenance (source,
   build, version, sha256). Stops if the managed AppImage already holds that
   build or a newer one (the `.appimage-build` record, or the same bytes).
2. **Download** into `~/.cache/okilum-deliver/<build>` before anything else, so
   a bad or slow payload never closes Okilum.
3. **Idle:** if Okilum is open (an `okilum` process holding
   `~/.local/state/okilum/reader-instance.lock`; `okilum sync serve` and the
   Sprint build never match), waits for 60 s without input through Wayland
   `ext-idle-notify-v1` (input idleness on notifier v2, so a playing video does
   not count as activity). It waits up to 25 min, then the next timer run
   tries again.
4. **Quit:** SIGTERM, which Okilum handles like Quit (editors saved, window
   state written). If it still runs 30 s later: SIGKILL. That loses nothing
   that autosave (1–2 s after typing) and the recovery journal do not already
   hold, because the user has been idle for at least 60 s (the reasoning of
   #2925 for the Mac).
5. **Install** with the updater's transaction: Okilum's install and update
   locks (an update the app has staged or is applying is never overwritten), a
   `.okilum-update-*` stage beside the AppImage with `previous.AppImage`,
   `expected.json` and `apply.log`, the build record, then a rename. Old stages
   are pruned like the updater does (the newest completed and the newest
   interrupted one are kept).
6. **Reopen** (only if it was open) in its own unit in `app-graphical.slice`
   (`systemd-run --user`, as uwsm launchers do), with
   `OKILUM_APPIMAGE_UPDATE_RECEIPT` set to the stage, so the new build itself
   confirms that it started. If it exits before that, the previous AppImage
   is put back and reopened; after two failures that build is skipped and the
   next one is tried.

Log, one line per action: `~/.local/state/okilum-deliver.log`, for example
`installed build 7760 source 1a2b3c4d5 · beta published 10:27:31 · seen 10:29:02 · idle wait 61s · quit TERM · installed 10:30:05 · running 10:30:09 (start confirmed by the app, …)`.
Each check also writes `~/.local/state/okilum-deliver/last-check.json` (feed
build, installed build, time). `okilum-beta-deliver --check` prints the feed
and installed build and changes nothing.

Install (as the desktop user, no sudo; from a checkout or copies of the three
files):

```bash
install -Dm755 scripts/dev/okilum-beta-deliver.py ~/.local/bin/okilum-beta-deliver
install -Dm644 scripts/dev/systemd/okilum-beta-deliver.service ~/.config/systemd/user/okilum-beta-deliver.service
install -Dm644 scripts/dev/systemd/okilum-beta-deliver.timer ~/.config/systemd/user/okilum-beta-deliver.timer
systemctl --user daemon-reload && systemctl --user enable --now okilum-beta-deliver.timer
```

Remove:

```bash
systemctl --user disable --now okilum-beta-deliver.timer; systemctl --user stop okilum-beta-deliver.service
rm -f ~/.local/bin/okilum-beta-deliver ~/.config/systemd/user/okilum-beta-deliver.service ~/.config/systemd/user/okilum-beta-deliver.timer
systemctl --user daemon-reload; rm -rf ~/.cache/okilum-deliver ~/.local/state/okilum-deliver
```

Timing (measured 14.11 over builds 7745–7759): the hosted beta appears 18.6–45
min after the merge, median about 25 min; 8 of 12 were published within 27
min. Delivery adds at most 2.5 min (timer), the idle wait, and about 20 s to
quit, install and reopen. So the 30-minute target holds only when CI publishes
within about 27 min. The rest of the latency is the hosted Linux build, not
this script.

Needs python3 (stdlib only), gpgv, systemd-run and a Wayland compositor with
`ext-idle-notify-v1` (Hyprland has it).
