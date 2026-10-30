# Stable release mirror and changelog

GitHub Latest previously stopped at `v0.1.13890` because the independent
`mac_only`, exact `windows_only`, and `linux_only` promotion paths returned
before the legacy combined GitHub publisher. The scheduled publisher refreshes
Beta only. Mirror/auth configuration is shared with the working Beta path;
this incident does not establish a token failure.

`releases.yml` now reconciles GitHub Latest **after a successful Stable
promotion**, including each split path. It reads the actual Stable feeds, not
Beta heads or the obsolete combined `releases/stable.json` marker. A failed
promotion, a Beta tick, and the separately frozen pacman path cannot run this
step. Existing GitHub App credentials (or the existing legacy fallback) are
used; no new secrets or permissions are introduced.

The release tag encodes the full independent platform matrix, for example
`stable-m18977-w19179-l7198`. Its anchor is the already mirrored macOS source;
each platform's source and version are explicit in the body and receipt. All
catalog payloads are checked for size/SHA256 before any GitHub write. Heads
are reread before publication to reject mixed snapshots. Existing matrix tags
must identify that same source. GitHub Latest, its notes, and uploaded asset
sizes/digests are read back after publication. Other releases remain available.
No Stable update feed or artifact is rewritten by this mirror.

Mac-only and exact Windows-only promotion now persist canonical Stable
`notes.json` before moving their feeds. These are the same per-platform
sidecars used by the in-app “What's new in this build”. AppImage and combined
promotion already do this. The initial historical fallback is restricted to
exact identities in `scripts/releases/stable-history.json`: the existing Mac
appcast descriptions and Windows per-build notes, with evidence URLs and
SHA256. Missing historical text is not reconstructed from guessed commits.

`CHANGELOG.md` is generated from that cumulative history. The backfill includes
published Mac Stable entries since 18 October, the corroborated Windows Stable
17312/19179 records, AppImage 7198, and the original 18 October cross-platform
GitHub release body. Empty existing notes are explicitly identified. The
historical Windows rows preserve per-build text, not an invented cumulative
Stable range. The legacy pacman release is retained in the original baseline;
this change does not promote or unfreeze pacman.

Refresh the checked-in history and changelog for a subsequent reviewed commit:

```sh
python3 scripts/releases/stable_github.py --changelog
```

This mode reads public feeds/sidecars and writes only local repository files;
it never constructs GitHub credentials or publishes. New current Stable rows
are appended to the retained history. Commit the resulting files through the
normal PR process; publication credentials do not bypass protected main.
Public GETs explicitly use `User-Agent: curl/8` and `Cache-Control: no-cache`:
the CDN rejects Python's default User-Agent with HTTP 403.

For the initial owner-authorized repair **after merge**, dispatch `releases.yml`
on `main` with only `sync_stable=true`. It excludes all build/source/promotion
inputs, takes the existing Stable publication lock, and performs no promotion
or native build. It publishes one matrix release and uploads
`stable-github-receipt.json` as an Actions artifact. A failed publication or
readback is an operational STOP, not an implicit retry.
