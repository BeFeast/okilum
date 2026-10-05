# Windows diagnostic (#432)

This portable Windows 11 x64 build is a read-only Reader diagnostic. Extract the
entire ZIP (including `gpui-shaders`), then open `tessera.exe`. Use Open Folder to select your vault, or drag a file
or folder onto the executable. Command-line examples:

```powershell
.\tessera.exe "C:\Users\Oleg\Obsidian Vault"
.\tessera.exe --vault "C:\Users\Oleg\Obsidian Vault" --note "Dev/Example.md"
```

Brain, managed workspace, export, note creation, rename/move and source editing are unavailable. Ctrl+E
shows "Editing is not available on Windows yet". No Windows update feed is used.
The header's More (⋯) menu provides Open file/folder, Appearance, About and Quit.
Its update notice says "Windows updates are not available yet — download the new ZIP".
The executable is unsigned; this first diagnostic does not claim SmartScreen
signing, file associations or native Windows acceptance.

Settings use the Windows roaming Known Folder (`%APPDATA%`); Reader history and
search indexes use the local Known Folder (`%LOCALAPPDATA%`). Notes remain the
canonical read-only source. Editor crash recovery is disabled in this build.

## Unreadable items

An unreadable child entry does not stop inventory, search or backlinks for the
readable part of the vault. The header shows "N items unreadable"; click it for
paths and errors, Copy details, and Retry after correcting access. Partial counts
are marked explicitly. Link verification stays uncertain while inventory is
incomplete; unreadable known note identities are retained to avoid false uniqueness.
The latest warning/failure report is `%LOCALAPPDATA%\tessera\reader-diagnostic.log`,
outside the vault. A missing or unreadable vault root still fails the open.

Preparation now reports the operation and full cause chain for cache validation,
Tantivy metadata/writer/commit, completion markers, and generation publication.
An external cache failure leaves inventory and backlinks ready and builds search
in memory for up to 128 MiB of readable source text. Above that fallback limit,
reading, note-name search and backlinks remain available while content search
reports unavailable. A failed watcher also leaves notes available, with an explicit warning
that automatic refresh is unavailable; use Retry to refresh. These warnings share
the clickable path list and diagnostic log. Windows verbatim path prefixes are
kept for filesystem I/O and omitted from user-visible paths/error chains.

## First bragi QA

Oleg confirmed launch, note opening, typography/markup, Properties, Contents and
sidebar sections on Windows 11. The first artifact failed preparation on an
unreadable entry, lacked toolkit icons and did not show caption controls. This
revision addresses those findings; native revalidation is still required.

Oleg also confirmed tree, tables, Contents and the More menu in Documents.
The remaining vault failure is specific to an entry in Obsidian Vault. Recent
and Pinned icons render because their assets were already embedded; toolbar,
tree and caption SVGs now use the same portable embedding policy. More includes
About and Quit on Windows, and managed tooltips yield while menus are open.

The follow-up artifact restored header/tree icons and visible caption controls
on the supplied screenshot. Obsidian Vault still failed preparation with only
the outer error context; this did not identify an entry or operation. The current
revision adds operation diagnostics and separates optional cache/watch failures
from readable inventory publication. Its native revalidation is pending.

## Check on bragi

1. Extract and launch; open your vault, including paths with spaces and Cyrillic.
2. Check body/code fonts, Cyrillic and emoji, selection and Ctrl+C.
3. Read tables, fenced code and local images (including filenames with spaces,
   `#` and `%`).
4. Follow wiki, relative and heading links; use Back/Forward and check the restored
   position. Ambiguous and missing links should remain explicit.
5. Use Ctrl+K, Ctrl+Shift+F and Ctrl+F; switch RU/EN input and open results.
6. Scroll, resize and maximize at 150% display scale; try Ctrl+E and confirm the
   unavailable-editing notice. External note edits should refresh the Reader.

Record actual results before estimating daily viewer, native safe save or updates.

## Build

On Linux install Rust 1.96.1 with `x86_64-pc-windows-msvc`, clang/lld/llvm,
`cargo-xwin` 0.23.1 and CairoSVG 2.8.2. Prepare and verify the pinned vendor using
`scripts/vendor-setup.sh`, then run `scripts/build-windows-ci.sh`.

The optimized `windows-diagnostic` profile inherits release settings and enables
GPUI's existing runtime HLSL compiler through debug assertions. Offline release
shaders require a Windows-hosted `fxc.exe` build script in this GPUI version; no
GPUI core patch is applied. The compiler wrapper supplies a relative shader
source path, and the ZIP includes the original pinned HLSL and its license. The LLVM resource wrapper preserves crate-relative manifest lookup.
Toolkit icons are explicitly embedded even when debug assertions are enabled.
Normal Linux and macOS release profiles are unchanged.
The CI job uploads a ZIP and checksum as artifacts, without feed publication.
