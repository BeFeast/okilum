# Bundled Sync Help

`setup.md` is the in-app written guide for the current Linux direct setup,
including fresh installations and explicit existing Syncthing reuse. `index.html` and its relative assets are
an offline video companion, adapted from the user-supplied v6 source bundle.
Media bytes are unchanged; `provenance.json` records their SHA-256 values.
The original v1/v5 evidence is preserved outside this repository.

The video shows an existing pair, not first connection. It has English visual
captions, a complete text transcript, native playback controls and a labelled
wait cut. It is neither latency evidence nor Oleg acceptance.

`reader_sync_help::entry()` opens the existing GPUI sheet pattern without Sync
requests. The optional video action publishes these immutable bundled files to
a content-specific directory under the existing application state location and
opens a file URL. It never depends on expiring hosted content.

Parent integration: register `mod reader_sync_help;`; the Sync Settings owner
adds `.child(crate::reader_sync_help::entry())` near its introduction. No new
service, daemon, network permission or global Help-menu route is required.

Run `python3 docs/help/sync/check.py` for the offline asset-closure and media
provenance checks. Native playback and UI acceptance remain separate.
