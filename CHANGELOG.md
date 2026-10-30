# Changelog

Stable releases use independent build numbers for each platform. Entries below retain existing published release text.

Existing published Mac Stable appcast entries since 2026-10-18; Windows signed Stable 17312 and 19179 corroborated by promotion receipts; AppImage Stable 7198. Historical missing Stable sidecars are not reconstructed from guessed commit ranges. Windows entries retain exact existing per-build notes, not a claimed cumulative Stable range.

## macos 0.1.18977 — Stable

Source: `1d6b3d868de8c66f5b400ab3389389ebd7c768b6`. [Release data](https://updates.befeast.com/okilum/appcast.xml).

- add WS v2 thread transport for the Linux Manager ([#1805](https://git.oklabs.uk/BeFeast/okilum/pulls/1805))
- virtualize the Manager feed ([#1797](https://git.oklabs.uk/BeFeast/okilum/pulls/1797))
- allow safe case-only note and folder renames ([#1745](https://git.oklabs.uk/BeFeast/okilum/pulls/1745))
- reveal notes beside their document breadcrumb ([#1780](https://git.oklabs.uk/BeFeast/okilum/pulls/1780))
- reconnect Manager with its saved T3 token and show renewal guidance ([#1776](https://git.oklabs.uk/BeFeast/okilum/pulls/1776))
- use AppImage build numbers for What’s new notes ([#1775](https://git.oklabs.uk/BeFeast/okilum/pulls/1775))
- isolate the explicit Sprint distribution ([#1761](https://git.oklabs.uk/BeFeast/okilum/pulls/1761))
- support current T3 tools without blocking Manager on optional features ([#1766](https://git.oklabs.uk/BeFeast/okilum/pulls/1766))
- accept bare T3 addresses in the Manager connection field ([#1757](https://git.oklabs.uk/BeFeast/okilum/pulls/1757))
- signed AppImage Stable and explicit final pacman freeze ([#1710](https://git.oklabs.uk/BeFeast/okilum/pulls/1710))
- retain downloaded admission on an authenticated collection ([#1712](https://git.oklabs.uk/BeFeast/okilum/pulls/1712))
- sweep visible projection styles once per prepaint ([#1748](https://git.oklabs.uk/BeFeast/okilum/pulls/1748))
- download evicted iCloud notes on explicit open ([#1747](https://git.oklabs.uk/BeFeast/okilum/pulls/1747))

## macos 0.1.18312 — Stable

Source: `e8ef13bca9553c5673d8a21819f4ff3be7680d51`. [Release data](https://updates.befeast.com/okilum/appcast.xml).

- paste clipboard images and refresh completed file drops ([#1744](https://git.oklabs.uk/BeFeast/okilum/pulls/1744))
- scale rendered code blocks with document zoom ([#1741](https://git.oklabs.uk/BeFeast/okilum/pulls/1741))
- update plain-folder sidebar after external file changes ([#1740](https://git.oklabs.uk/BeFeast/okilum/pulls/1740))
- preserve Read preference and sidebar focus when reopening notes ([#1736](https://git.oklabs.uk/BeFeast/okilum/pulls/1736))
- keep Trash available during a published warm vault refresh ([#1735](https://git.oklabs.uk/BeFeast/okilum/pulls/1735))

## macos 0.1.18013 — Stable

Source: `931107b646e363e6b38301f92ec56ed0b0200ef6`. [Release data](https://updates.befeast.com/okilum/appcast.xml).

- preserve Read preference and sidebar focus when reopening notes ([#1736](https://git.oklabs.uk/BeFeast/okilum/pulls/1736))
- keep Trash available during a published warm vault refresh ([#1735](https://git.oklabs.uk/BeFeast/okilum/pulls/1735))

## macos 0.1.17502 — Stable

Source: `35f375ffa6a3bef5a037aa0fe77e24f14938cdf6`. [Release data](https://updates.befeast.com/okilum/appcast.xml).

- remove filesystem link probes from note preparation ([#1705](https://git.oklabs.uk/BeFeast/okilum/pulls/1705))

## macos 0.1.16684 — Stable

Source: `be5988037faa44699c196292518921ac79681c1b`. [Release data](https://updates.befeast.com/okilum/appcast.xml).

- restore webview editing shortcuts and retain its visible frame ([#1539](https://git.oklabs.uk/BeFeast/okilum/pulls/1539))

## macos 0.1.13890 — Stable

Source: `9474934ae1cce9ab88f81ae924d35a9c46b53bca`. [Release data](https://updates.befeast.com/okilum/appcast.xml).

No user-visible items were recorded in the existing build notes.

## windows 0.1.19179 — Stable

Source: `e8ef13bca9553c5673d8a21819f4ff3be7680d51`. [Release data](https://updates.befeast.com/okilum/releases/e8ef13bca9553c5673d8a21819f4ff3be7680d51/windows/beta/notes.json).

No user-visible items were recorded in the existing build notes.

## windows 0.1.17312 — Stable

Source: `ef1930ca6dea87bd4d677c9a67a5579ac9477eb0`. [Release data](https://updates.befeast.com/okilum/releases/ef1930ca6dea87bd4d677c9a67a5579ac9477eb0/windows/beta/notes.json).

No user-visible items were recorded in the existing build notes.

## linux-appimage 0.1.7198 — Stable

Source: `62d8ed865888baa299f66ff05f1f0b93137328b8`. [Release data](https://updates.befeast.com/okilum/releases/62d8ed865888baa299f66ff05f1f0b93137328b8/linux-appimage/stable/notes.json).

- move Maximize to the note toolbar and remove the redundant panel title ([#1974](https://git.oklabs.uk/BeFeast/okilum/pulls/1974))
- bind macOS Enable confirmation to one launch generation ([#1972](https://git.oklabs.uk/BeFeast/okilum/pulls/1972))
- keep Send and Cancel visible in small windows ([#1973](https://git.oklabs.uk/BeFeast/okilum/pulls/1973))
- restore Ubuntu color emoji in notes and code ([#1954](https://git.oklabs.uk/BeFeast/okilum/pulls/1954))
- recover interrupted update links and prune old stages ([#1959](https://git.oklabs.uk/BeFeast/okilum/pulls/1959))
- count infra markers only on the runner's own lines (#1962) ([#1969](https://git.oklabs.uk/BeFeast/okilum/pulls/1969))
- show About for the current Reader window ([#1957](https://git.oklabs.uk/BeFeast/okilum/pulls/1957))
- clean failed unpublished saves and preimage links ([#1956](https://git.oklabs.uk/BeFeast/okilum/pulls/1956))
- use verified appimagetool release 1.9.1 ([#1965](https://git.oklabs.uk/BeFeast/okilum/pulls/1965))
- an unreadable lane log is unknown, not infrastructure (#1937) ([#1961](https://git.oklabs.uk/BeFeast/okilum/pulls/1961))
- widen only date: values; keep notes indexed when a read fails (#1933) ([#1952](https://git.oklabs.uk/BeFeast/okilum/pulls/1952))
- scroll opening space without trackpad jumps ([#1911](https://git.oklabs.uk/BeFeast/okilum/pulls/1911))
- support Insert clipboard shortcuts in feedback and editor ([#1928](https://git.oklabs.uk/BeFeast/okilum/pulls/1928))
- Show cached project icons before network refresh ([#1946](https://git.oklabs.uk/BeFeast/okilum/pulls/1946))
- use vault Trash when network volume has no system Trash ([#1929](https://git.oklabs.uk/BeFeast/okilum/pulls/1929))
- Explain macOS local network connection failures and retry once ([#1931](https://git.oklabs.uk/BeFeast/okilum/pulls/1931))
- support NFS writes without exclusive rename flags ([#1916](https://git.oklabs.uk/BeFeast/okilum/pulls/1916))
- Fix connected T3 panel connection controls ([#1919](https://git.oklabs.uk/BeFeast/okilum/pulls/1919))
- convert pasted and imported images to WebP ([#1896](https://git.oklabs.uk/BeFeast/okilum/pulls/1896))
- preserve generated vendor bytes across workspace formatting ([#1906](https://git.oklabs.uk/BeFeast/okilum/pulls/1906))
- choose note and code font families ([#1895](https://git.oklabs.uk/BeFeast/okilum/pulls/1895))
- compose native macOS registration without nested locks ([#1889](https://git.oklabs.uk/BeFeast/okilum/pulls/1889))
- use Windows OS trust and system proxy settings ([#1698](https://git.oklabs.uk/BeFeast/okilum/pulls/1698))
- gate macOS daemon launch on durable one-use intent ([#1888](https://git.oklabs.uk/BeFeast/okilum/pulls/1888))
- verify prepared macOS payload without starting it ([#1886](https://git.oklabs.uk/BeFeast/okilum/pulls/1886))
- confirm supervisor exit after authenticated Stop ([#1885](https://git.oklabs.uk/BeFeast/okilum/pulls/1885))
- create and manage project cards in place ([#1884](https://git.oklabs.uk/BeFeast/okilum/pulls/1884))
- observe authenticated macOS supervisor exit ([#1876](https://git.oklabs.uk/BeFeast/okilum/pulls/1876))
- prefer TOML manifests and keep invalid Board cards local ([#1878](https://git.oklabs.uk/BeFeast/okilum/pulls/1878))
- restore from protected macOS Trash without directory reads ([#1881](https://git.oklabs.uk/BeFeast/okilum/pulls/1881))
- derive In progress from open PRs and the executor current task ([#1866](https://git.oklabs.uk/BeFeast/okilum/pulls/1866))
- add verified macOS supervisor connection ([#1868](https://git.oklabs.uk/BeFeast/okilum/pulls/1868))
- share app sign-in and recover timed-out browser callbacks ([#1871](https://git.oklabs.uk/BeFeast/okilum/pulls/1871))
- keep cached graph snapshots complete and raise test nofile ([#1870](https://git.oklabs.uk/BeFeast/okilum/pulls/1870))
- give the vault start page clear actions and aligned recent notes ([#1864](https://git.oklabs.uk/BeFeast/okilum/pulls/1864))
- verify macOS bundle binding and static code signatures ([#1863](https://git.oklabs.uk/BeFeast/okilum/pulls/1863))
- Add Maximize Note action with reversible sidebar layout ([#1850](https://git.oklabs.uk/BeFeast/okilum/pulls/1850))
- accept T3 launch manager IDs without route escapes ([#1855](https://git.oklabs.uk/BeFeast/okilum/pulls/1855))
- share daemon inspection and folder controllers with macOS ([#1853](https://git.oklabs.uk/BeFeast/okilum/pulls/1853))
- avoid legacy bridge ref directory collisions ([#1856](https://git.oklabs.uk/BeFeast/okilum/pulls/1856))
- expose durable enrollment on macOS ([#1849](https://git.oklabs.uk/BeFeast/okilum/pulls/1849))
- send native T3 commands and answer manager requests ([#1848](https://git.oklabs.uk/BeFeast/okilum/pulls/1848))
- retain safe GitHub bridge push and cleanup diagnostics ([#1847](https://git.oklabs.uk/BeFeast/okilum/pulls/1847))
- Add explicit Download all for iCloud placeholders ([#1846](https://git.oklabs.uk/BeFeast/okilum/pulls/1846))
- keep iCloud placeholders pending (slice 1 of 2) ([#1843](https://git.oklabs.uk/BeFeast/okilum/pulls/1843))
- prefer color emoji in Reader and Source/Live Preview ([#1840](https://git.oklabs.uk/BeFeast/okilum/pulls/1840))
- Opening a missing Markdown path creates an empty note and opens it for editing. ([#1841](https://git.oklabs.uk/BeFeast/okilum/pulls/1841))
- The file tree shows file names by default; switch to note titles in Settings without restarting. ([#1838](https://git.oklabs.uk/BeFeast/okilum/pulls/1838))
- create notes and folders in plain-folder mode ([#1830](https://git.oklabs.uk/BeFeast/okilum/pulls/1830))

## 0.1.13890 — published cross-platform release (2026-10-18)

[Original release](https://github.com/BeFeast/okilum/releases/tag/v0.1.13890)

Same-source builds — macos: 13890, windows: 13678, linux: 13846.

- fix(open-in): Cursor opens the note in a new editor window (#1398) ([#1403](https://git.oklabs.uk/BeFeast/okilum/pulls/1403))
- fix(open-in): Cursor opens the note in an editor window (#1398) ([#1401](https://git.oklabs.uk/BeFeast/okilum/pulls/1401))
- docs: bug report form and CONTRIBUTING.md with log locations (#1396) ([#1397](https://git.oklabs.uk/BeFeast/okilum/pulls/1397))
- feat(reader): create a new vault from the start screen (#1357) ([#1395](https://git.oklabs.uk/BeFeast/okilum/pulls/1395))
- fix(windows): copy and show plain paths, not \\?\C:\ (#1391) ([#1392](https://git.oklabs.uk/BeFeast/okilum/pulls/1392))
- fix(test): two reader_loading timing flakes; ci-logs checks tests first ([#1229](https://git.oklabs.uk/BeFeast/okilum/pulls/1229))
- fix(reader): sidebar empty sections read as one set of hints (#1350) ([#1393](https://git.oklabs.uk/BeFeast/okilum/pulls/1393))
- fix(reader): each window of a vault comes back on its own note (#1375) ([#1383](https://git.oklabs.uk/BeFeast/okilum/pulls/1383))
- fix(editor): a refresh during an autosave is not a conflict (#1376) ([#1380](https://git.oklabs.uk/BeFeast/okilum/pulls/1380))
- fix(reader): the start screen names a missing vault in plain words (#1368) ([#1377](https://git.oklabs.uk/BeFeast/okilum/pulls/1377))
- fix(create): a new note is just its title and opens in Live Preview (#1352) ([#1381](https://git.oklabs.uk/BeFeast/okilum/pulls/1381))
- ci(releases): promote Mac and Linux before the Windows signature ([#1379](https://git.oklabs.uk/BeFeast/okilum/pulls/1379))
