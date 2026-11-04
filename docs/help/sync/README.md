# Bundled Sync Help

`setup.md` is the in-app written guide for the current Linux direct setup,
including fresh installations and explicit existing Syncthing reuse. `index.html` and its relative assets are
an offline video companion, using the owner-supplied first-connection tutorial.
Media bytes are unchanged; `provenance.json` records their SHA-256 values.
The original recordings, build script and segment list are preserved outside this repository; their paths and recording details are in the manifest.

The 1:18 tutorial shows first connection on two fresh Linux installations with
English narration (ElevenLabs Alice), a WebVTT track, matching inline captions
for local file playback, a complete transcript and native playback controls.
Waiting is cut and some steps are sped up to 3×; one folder-dialog line is masked.
It is neither latency evidence nor Oleg acceptance.

`reader_sync_help::entry()` opens the existing GPUI sheet pattern without Sync
requests. The optional video action publishes these immutable bundled files to
a content-specific directory under the existing application state location and
opens a file URL. It never depends on expiring hosted content.

Parent integration: register `mod reader_sync_help;`; the Sync Settings owner
adds `.child(crate::reader_sync_help::entry())` near its introduction. No new
service, daemon, network permission or global Help-menu route is required.

Run `python3 docs/help/sync/check.py` for the offline asset-closure and media
provenance checks (requires `ffprobe`, provided by FFmpeg). Native playback and UI acceptance remain separate.

## Public site

This repository has no pipeline publishing `docs/help` to the public site.
`okilum.app` is a separate static-assets project at `~/src/okilum-site`, deployed
with `python3 build.py` and `wrangler deploy` as Cloudflare Worker `okilum-site`;
it does not currently serve this Help page. The owner already published this
same tutorial at <https://html.me.uk/t/okilum-sync-tutorial/index.html>.
Its `okilum-sync-tutorial.mp4` matches the bundled SHA-256.
