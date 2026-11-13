# User keyboard shortcuts (#2626)

Settings → **Keyboard Shortcuts** lists actions, their contexts and current
shortcuts, with Default / Modified labels. Search accepts an action name, context
or combination. Record captures the next key press without invoking its existing
action; modifiers alone are ignored. Esc cancels recording and keeps Settings
open. Confirm assigns the captured combination; it does not save automatically.
The recorder can use Global, ReadMode or Editor; existing specialized contexts
are retained unless another context is selected. An action can have several combinations. Clicking a combination disables it;
Reset restores that action/context, and Reset all removes the user overrides.

Assigning a combination already used in the same context offers **Reassign
shortcut** or Cancel. Reassign removes the old assignment in that context.
Resetting an action also asks before taking a default key from another modified
action. Overlapping broader/global contexts produce warnings; ReadMode and
Editor can use the same combination independently. Ctrl+Alt on Windows warns
about AltGr, and commonly reserved OS/window-manager combinations have a soft
warning. Okilum cannot capture a combination the OS never delivers.

## Quote formatting

**Quote (#2625):** Format → Quote, also in the editor's context menu, toggles
one outer blockquote level with Control+Shift+Q on every OS. It applies to the current line
or every line touched by a selection in Source and Live Preview; an ending at
the next line's start leaves that line alone. When every selected line already
has a `>` marker, it removes one level. Otherwise it adds `> ` to every selected
line, including blank lines and already quoted lines. Text, indentation, BOM
and line endings are retained. Each toggle is one Undo step and does not save
the file. Frontmatter and selections with boundaries inside fenced code are
left unchanged, matching the other formatting commands' source guards.
The `okilum::ToggleQuote` action uses the Editor context and can be rebound here.

## File and precedence

`keymap.json` lives beside the other JSON preferences, under
`config_base()/okilum/`. **Open keymap file** opens it in the OS's configured
editor, creating the current overrides on the first explicit request if needed.

```json
{
  "version": 1,
  "bindings": [
    {
      "context": "Editor",
      "key": "ctrl-alt-v",
      "action": "input::Paste"
    },
    {
      "context": "Editor",
      "key": "shift-insert",
      "action": "input::Paste"
    },
    {
      "context": "ReadMode",
      "key": "ctrl-k",
      "action": null
    }
  ]
}
```

Use the exact action ID shown by Settings; IDs come from GPUI's action registry.
An optional `args` value supplies parameters for actions that require them.
Keys use GPUI notation, including platform-aware `secondary` and space-separated
chords. A missing context means Global. The aliases are:

| Context | GPUI predicate |
|---|---|
| Global | No predicate |
| ReadMode | `ReaderRead && !Input && !ReaderTree` |
| Editor | `ReaderSource > Input` |

Explicit GPUI predicates are also accepted and are listed beside their actions.
Overrides replace defaults for the exact context/key pair; null disables that
combination. Other defaults remain. More specific contexts follow GPUI's native
precedence. The file is checked once per second off the UI executor and updates
without restart. Removing it restores defaults. A stale background read cannot
replace a newer accepted configuration.

The complete file is validated before changing the active keymap. Invalid JSON,
unknown actions, bad keys/predicates, duplicate context/key pairs, unsupported
versions and oversized files show an error in Settings while the last working
bindings remain active. The file is not rewritten to hide errors. Settings saves
are serialized between Settings windows, use an atomic replacement and refuse to overwrite a file changed externally since
it was loaded. Fix/reload an invalid file before editing it through Settings.

Menus, the shortcut sheet and hints use the active GPUI keymap; native application
menus are refreshed after a change. There is no gpui core patch. Note content,
vault files, OS shortcuts and other applications' configuration are untouched.

## Verification

The `keymap_2626_` tests exercise actual GPUI binding resolution and recorded-key
dispatch, Cancel/Reassign, aliases, null and multiple bindings, independent
contexts/global warnings, last-good behavior, atomic save conflict refusal,
search/reset and same-session file reload. Native platform checks remain separate
from installed tester acceptance; delivery is recorded with the first containing
beta per OS in issue #2626.

## Code and data views (#2809)

In a read-only code/data viewer, Alt+1…4 selects Table, Tree, Formatted and Raw;
on macOS use Control+1…4 to avoid the Option text-input layer. A view the file
does not offer is a no-op. Ctrl+Shift+[ / ] folds/unfolds the active view without
switching views. Linux accepts both unshifted bracket+Shift and the reported
shifted character; macOS accepts Control+{ / } as reported by its event layer.
Ctrl+Shift+C copies a node in the code context; notes retain Copy as Markdown
on their existing platform shortcut. Alt/Option+PageUp and PageDown selects the
previous/next text part. All commands are listed and rebindable; hints use the
live keymap. macOS Mission Control priority requires installed-beta verification.

### Code/data Tree rows

In a read-only code/data file Ctrl/Cmd+F focuses the shared header field, except
JSON Table, which retains its own filter until its toolbar conversion. Enter and
Shift+Enter step; Esc clears and leaves Find. Tree owns Up/Down, Right to expand
or enter a child, Left to collapse or select its parent, and Enter to show the
selected node in Raw. Copy node uses Ctrl+Shift+C on Linux, Windows **and Mac**;
Copy as Markdown keeps its existing note contexts. Row menus/tooltips resolve
these commands from the active keymap.

The Mac Copy node binding intentionally follows parent manager default 17
(Ctrl+Shift+C), overriding #2810 Original item 14's Shift+Cmd+C wording. The
platform acceptance uses this recorded default and the live keymap.

## File code operations

File Edit adds these defaults to the live shortcut catalog and Format split
menu. They also work with focus in the header Find field. Markdown fenced-code
controls keep their existing host and bindings.

| Action | Linux / Windows | macOS |
|---|---|---|
| Format code | Shift+Alt+F | Shift+Option+F |
| Validate code | Shift+Alt+V | Shift+Option+V |
| Repair code… | Shift+Alt+R | Shift+Option+R |

Repair from a code/data read view enters Edit before opening its lower preview.
The OS may consume Alt+Shift for keyboard-layout switching before Okilum receives
the chord; Settings can rebind each command. Actual layout behavior must be
recorded during installed-beta platform verification, separately from GPUI tests.
