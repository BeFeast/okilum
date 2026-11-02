# Stable release procedure — 9 October 2026

Historical all-platform procedure, superseded as process policy by **Okilum —
how we work (v3, 02.11)** (see [AGENTS.md](../AGENTS.md)). Current per-platform
promotion mechanics are in [releases.md](releases.md). The same-source steps below
describe this older all-platform path, not a gate on daily per-platform Stable.

1. Freeze one accepted `main` commit S. Record required CI success and QA evidence
   on muninn (Linux) and bragi (Windows), plus the signed macOS packaged smoke test.
   Record the exact source and platform build numbers; do not infer equivalence
   from the rolling `beta` tag.
2. Obtain successful published beta artifacts for **the same S** on macOS,
   Windows and Arch. Main macOS run 4107 selected `102b64aa53696a7740c7632d8eabee44647221d4`
   (build 9107); it is only a candidate, not yet an accepted stable selection.
   If another source is selected, all three catalog records must match it.
   Branch PR800 artifacts are QA-only and cannot be promoted by this procedure.
3. Verify catalog assets and SHA256: signed/notarized `Okilum-macos.zip`, Windows
   `Setup.exe` and `Okilum-windows-portable.zip`, Arch `.pkg.tar.zst` and detached
   signature. Verify the Sparkle enclosure signature, Velopack archived feed and
   Arch metadata/signature. Keep the original platform version numbers.
4. Dispatch `releases.yml` on `main` with
   `build=<accepted macOS beta build>`, `supersede_pending=false`.
5. The existing coordinator tags S as `v0.1.<macOS build>`, mirrors that tag,
   promotes existing artifacts without rebuilding to Sparkle stable, the signed
   pacman stable repository and Velopack stable. GitHub publishes the same asset
   set plus `SHA256SUMS`, non-prerelease and **Latest**. Signing and secrets do not change.
6. Read back GitHub Latest/tag/source/assets, `okilum/releases/stable.json`,
   Sparkle stable entry, Arch stable latest.json/repository and Windows stable
   releases.stable.json/Setup.exe. Verify hashes and update checks on accepted
   installed versions. Retain the promotion manifest and release run URL.

Feeds cannot be changed atomically. An interrupted promotion retains its exact
selection in `okilum/releases/promoting.json`; resume the same build. Do not
roll back feeds or silently pick a newer source. A different recovery selection
uses the existing explicit supersede procedure and its recovery checks.
