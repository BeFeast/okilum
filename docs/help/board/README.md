# Board Help (#2357)

`setup.md` is the contextual written guide; `index.html` presents the same steps,
a 1:45 continuous production-app recording, transcript and saved-state capture.
All playback resources are relative and bundled. Reading Help requires neither a
repository nor a Manager connection. The page offers ordinary external site
navigation; playback and instructions remain offline.

The recording uses **Linux beta 0.1.7764**, source
`e2a6e8556aa2b974a0ddd4012b93ec77d746bfd7`, from the published, checksum-verified
AppImage. It shows every step from enabling Agents through creating and opening a
project/Board to persisting Todo → Doing. `provenance.json` records the artifact,
media checksums and capture/editing method. `events.json`, `actions.py`, the
before/after TOML and the final actual UI frame retain action/save evidence.
The application state, vault and display are private tutorial fixtures. No
production project, T3 server or repository was used. This is actual GPUI UI,
not a web recreation. Software rendering is not performance evidence.

English captions occupy an added band **below** the unmodified 1440×960 UI;
there are no cuts, speed changes or narration. The separate VTT and full HTML
transcript provide accessible text. Original frames, the session launcher and
raw recordings remain in `/home/god/work/qa-evidence/issue-2357/`.
Manrope is distributed under the adjacent SIL Open Font License.

The Help button is mounted before Board creation, in List and in Board view.
It opens the existing sheet route. The optional video button exports a verified,
content-addressed bundle outside the vault/index using the same immutable
publisher as Sync Help. Failed export retains the written guide and copy action.

## Website integration

`okilum.app` has its own static source tree; product docs are not published there
by the application build. Install the canonical guide into that tree:

```sh
python3 docs/help/board/install-site.py /path/to/okilum-site
```

This copies the exact playback bundle to `src/help/board`, adds an idempotent
copy hook to the existing `build.py`, and exposes Board help in both existing
footer templates. It preserves the site's release/download settings. Then use
the site's normal build and Cloudflare deployment instructions. Future content
updates use the same installer and deployment; application release is separate.
Public destination: <https://okilum.app/help/board/>. Published on 14 November
2026 as Cloudflare version `942ad915-360a-4e2f-9413-941b25423c78`; website source
commit `88509361da50b1a6f0ea8e7ac53932849d1cfbe7` on `docs/2357-board-guide`.
All six published files were downloaded and matched byte for byte to this
bundle; the home and download footers expose the guide.

## Verification

```sh
python3 docs/help/board/check.py
cargo fmt --all -- --check
H=github scripts/agent/gh-check.sh
```

The Python check verifies asset closure (with rejected nonlocal/missing resource
controls), exact media/source provenance, transcript coverage, matching written
instructions, a real same-card saved state change and idempotent site integration.
Hosted Rust tests click Help in all three Board states and require the sheet to
appear without opening a URL or writing project files. The shared publisher's
controls refuse changed assets and replaced directories. Native playback and
human acceptance are distinct from those checks.
