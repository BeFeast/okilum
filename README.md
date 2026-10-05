<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/images/readme/hero-dark@2x.png">
    <img src="docs/images/readme/hero-light@2x.png" width="100%"
         alt="Tessera — a native desktop app for Markdown notes. Your Markdown vault, open in a second. Open source, MIT, macOS, Linux, Windows.">
  </picture>
</p>

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/images/readme/features-dark@2x.png">
    <img src="docs/images/readme/features-light@2x.png" width="100%"
         alt="Plain files on your disk. Instant on big vaults. Reads your whole vault: wikilinks, backlinks, properties, tables, code, SVG, Excalidraw. Edits you can undo: atomic saves, note history, rename that updates every link.">
  </picture>
</p>

# Tessera

Tessera is a fast, native desktop app for a folder of Markdown notes — an
Obsidian-style vault. Your notes stay ordinary files on your disk: no account,
no server, no import. Indexes and previews are rebuildable caches.

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/images/readme/shot-reader-dark@2x.png">
    <img src="docs/images/readme/shot-reader-light@2x.png" width="100%"
         alt="Read your vault the way you wrote it. Tree, properties, contents and backlinks around every note. Screenshot of Tessera with a folder tree, a rendered note with an image and a specifications table, and a right panel with Properties, Contents and Linked from.">
  </picture>
</p>

## What it does

- **Reads your whole vault.** Wikilinks and backlinks with context, frontmatter
  properties, wide tables, code with language labels, images, SVG and native
  Excalidraw drawings inside notes. Obsidian-style links with spaces, `%20` and
  absolute paths resolve the same way everywhere.
- **Finds things fast.** ⌘K quick open, full-text search, hover previews of
  linked notes, a table of contents that follows you.
- **Edits safely.** Source + preview editing with atomic saves, per-note history
  with restore, and rename/move that updates every link after showing you the
  exact changes.
- **Stays out of your way.** Opens the last vault and note on launch, remembers
  window layout, and updates itself.

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/images/readme/shot-tables-dark@2x.png">
    <img src="docs/images/readme/shot-tables-light@2x.png" width="100%"
         alt="Long notes stay navigable. Wide tables, a contents panel that follows you, and every place a note is linked from. Screenshot of a long note with a wide scrollable table, the current section highlighted in Contents, and a Linked from list with excerpts.">
  </picture>
</p>

## Download

| Platform | Status | How to install |
|---|---|---|
| macOS (Apple Silicon) | Beta, signed and notarized, auto-updates | [macOS installation and updates](docs/macos-auto-update.md) |
| Arch Linux (x86_64) | Beta, signed pacman repository | [Arch Linux packages](docs/linux-releases.md) |
| Windows (x64) | Preview, read-only | [Windows preview](docs/windows-diagnostic.md) |

Tessera is under active development. Keep normal backups of your notes.

## Build from source

See [platform prerequisites and build instructions](docs/building.md).
With Rust 1.96.1 and your platform libraries installed:

```sh
git clone https://github.com/BeFeast/tessera.git
cd tessera
bash scripts/vendor-setup.sh
bash scripts/vendor-setup.sh --verify
cargo +1.96.1 build --release --locked -p tessera-shell
./target/release/tessera --vault /path/to/notes
```

The vendor script downloads the pinned gpui-component source and applies our
tracked patches; do not build against an unpatched upstream checkout. macOS also
needs the pinned Sparkle framework — follow the platform guide.

## Contributing

Bug reports and ideas are welcome in [GitHub issues](https://github.com/BeFeast/tessera/issues).
Pull requests are welcome too: development happens on a private Forgejo
instance and this repository mirrors `main` and releases, so accepted changes
are landed there and appear here with your authorship preserved.

The [product contract](docs/PRD.md) and [Reader design](docs/design/reader.md)
describe how Tessera is meant to behave. The workspace is split into
`tessera-core` (files, parsing, indexing, rendering model) and `tessera-shell`
(the GPUI desktop app).

## License

Tessera is licensed under the [MIT License](LICENSE). Dependencies and bundled
fonts and icons keep their own licenses — see
[third-party notices](THIRD_PARTY_NOTICES.md) and
[license sources](licenses/README.md).
