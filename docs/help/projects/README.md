# Projects Help (#2356)

`setup.md` is the contextual written guide; `index.html` presents the same steps,
a 2:15 continuous production-app recording, transcript and the final saved-state
frame. All playback resources are relative and bundled. Reading Help requires
neither a project nor a Manager connection, and it writes no vault files.

The recording uses **Linux beta 0.1.7838**, source
`aef9f091571c5c1367509b05ef6fe0ce2ea5052e`, from the published, checksum-verified
AppImage. It shows every step from turning on Agents, through creating a local
project from an existing folder, its Board, Decisions and Reports tabs and the
return to Files, to reopening the project from the top bar with its saved card.
`provenance.json` records the artifact, media properties and file checksums.
`session.sh` (isolated state, synthetic practice vault, private Xvfb display),
`actions.py` (cursor actions and caption timing), `events.json` and the
recorded manifest/card files keep the capture reproducible. This is actual GPUI
UI, not a web recreation. Software rendering is not performance evidence.

English captions occupy an added band **below** the unmodified 1440×960 UI;
there are no cuts, speed changes or narration. The VTT file and the HTML
transcript provide accessible text. Manrope is distributed under the adjacent
SIL Open Font License.

The Help button sits in the Projects section header, beside the title, so it is
available before the first project exists. It uses the shared contextual Help
sheet (`crates/okilum-shell/src/contextual_help.rs`, also used by Board Help);
the video button exports the same immutable, content-addressed bundle outside
the vault.

The guide stays inside what the recorded build does locally. Manager, T3
projects, agents, linked issues and creating records are named as optional or
unavailable and are not shown.

## Website integration

`okilum.app` has its own static source tree. Install the canonical guide there:

```sh
python3 docs/help/projects/install-site.py /path/to/okilum-site
```

This copies the playback bundle to `src/help/projects`, adds the same idempotent
`help` copy hook as the Board installer (once), and adds **Projects help** to
both footer templates without touching other links or download settings. Then
use the site's normal build and Cloudflare deployment. Public destination:
<https://okilum.app/help/projects/> (live since 2026-11-17).

## Verification

```sh
python3 docs/help/projects/check.py
H=github scripts/agent/gh-check.sh
```

The Python check verifies asset closure (with rejected nonlocal/missing resource
controls), that the in-app bundle list equals the website file list, matching
written instructions, exact media/source provenance, caption/transcript
coverage, the recorded manifest and card, and that the Projects and Board
installers coexist in either order. The hosted Rust test clicks Help in the
Projects section with and without projects and requires the sheet to appear
without opening a URL or changing any vault file. Native playback and human
acceptance are separate from these checks.

## Exact-build capture

`exact-build/` holds frames of the in-app Help taken on 2026-11-20 from the
published Linux beta **0.1.7911** (source
`ecf83913b12effc11680b09df456d7f8c1f1e96a`, which contains the Help merge
`5d7fc2bc1`). The AppImage sha256 matched the beta feed. The session used the
same `session.sh` isolation and practice vault as the recording. The frames show
the Help sheet opened from the Projects header before the first project exists,
the end of the sheet (limits, troubleshooting, video button), Help again with one
project listed, and the project's Board, Decisions, Reports and Handovers tabs.
The practice vault was byte-identical before and after opening Help.
`exact-build/receipt.json` records the artifact, the steps and frame checksums;
`check.py` verifies them.
