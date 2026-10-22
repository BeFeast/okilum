# Mac sprint sessions

Run on an Apple Silicon Mac in Terminal:

```bash
curl -fsSL https://git.oklabs.uk/BeFeast/okilum/raw/branch/main/scripts/dev/okilum-sprint.sh -o /tmp/okilum-sprint.sh && bash /tmp/okilum-sprint.sh sprint/<topic>
```

Replace `sprint/<topic>` with the announced branch. The first build clones the
public repository and prepares dependencies. Subsequent builds reuse
`~/okilum-sprint/src` and `~/okilum-sprint/target`. Existing Rust, Python 3 and
Xcode command-line tools are required. The script does not install them.

`~/Applications/Okilum Sprint.app` has its own bundle ID, amber icon badge and
state under `~/Library/Application Support/com.befeast.okilum.sprint` (including
config and cache). The launcher applies isolation on every Finder launch. Choose
the vault through the normal picker. Vault files remain the same canonical files;
source edits made in Sprint affect that vault as usual.

The bundle uses the release script's Info.plist template, removes the update feed
and ordinary document/URL associations, and is ad-hoc signed. Its linked pinned
Sparkle framework is retained to satisfy the existing binary linkage, but no
updater controller is created without a feed. It cannot update the working app.
The script quits only the Sprint bundle before replacement and leaves the
previous Sprint app in `~/okilum-sprint/previous-Okilum-Sprint.app`.

Build errors are retained in `~/okilum-sprint/build.log`; paste the first error to
the manager. Local changes in the dedicated checkout stop the script before branch
replacement. A concurrent build stops at `~/okilum-sprint/build.lock`; after a
crash, remove that empty directory only when the earlier build has stopped.

Native acceptance is pending: launch alongside the working app, verify independent
state/updates and the visible icon, then time a second run after a one-line change.
The three-minute warm rebuild goal is unmeasured. No agent runs this script over SSH.
