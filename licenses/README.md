# Third-party license sources

Tessera's own code is MIT, not dual-licensed. Third-party code and assets retain
their licenses. `THIRD_PARTY_NOTICES.md` is the distributable combined notice.
Regenerate it with cargo-about 0.9.2 and `python3 scripts/third-party-notices.py`.
The generator uses all workspace features and platforms, including build-time
components; this is a conservative superset of any individual release.

- GPUI and gpui-kit: Apache-2.0; pinned by Cargo.lock and scripts/vendor-setup.sh.
  Tessera's changes are recorded in scripts/patches; retain those modification
  records with redistributed sources. No separate NOTICE exists in the pinned
  gpui-kit tree. Apache-2.0.txt is its complete license.
- Excalifont: SIL OFL 1.1. The pinned upstream index.ts contains the original font
  name table and full license. excalifont-provenance.json identifies all subsets;
  their cmap/glyf/hmtx/name tables match the bundled TTF conversions. Source:
  https://github.com/excalidraw/excalidraw/blob/21ffaf4d76bf8554c3ba5561ab8ff41ce89b55fa/packages/excalidraw/fonts/Excalifont/index.ts
- Virgil and Nunito: SIL OFL 1.1, from Excalidraw font subsets. Nunito's complete
  license and copyright: https://github.com/google/fonts/blob/main/ofl/nunito/OFL.txt
- Liberation Sans: bundled TTF is byte-identical to Excalidraw's
  scripts/woff2/assets/LiberationSans-Regular.ttf; no Tessera font modification.
  Copyright and OFL declaration are in its name table, reproduced with full OFL.
- Noto Sans and Cascadia Code: SIL OFL 1.1; notices and conversion provenance are
  in crates/tessera-shell/assets/brand/fonts and its manifest. The supplied
  Excalifont/Virgil/Nunito/Noto/Cascadia copyright declarations do not designate
  Reserved Font Names; format conversion/merging is documented, not relicensed.
  Font family names and trademarks are not an endorsement by their authors.
- Shell icons: Lucide ISC, plus Feather MIT for inherited icons (including
  arrow-up-right, clock, link and list). Lucide.txt retains both notices.
  Source: https://github.com/lucide-icons/lucide/blob/main/LICENSE
- Sparkle 2.10.0: Sparkle.txt includes its bundled third-party notices.
  Source: https://github.com/sparkle-project/Sparkle/blob/2.10.0/LICENSE

cargo-about selects the OR alternatives in about.toml and retains required AND
licenses. MPL-2.0 dependencies remain MPL: corresponding crate sources are
available from crates.io by the versions listed in the generated notice.
System libraries are provided by the operating system/package manager; their
packages retain their own licenses. This does not change their obligations.
