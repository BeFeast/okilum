# User keyboard shortcuts (#2626)

Settings → **Keyboard Shortcuts** lists actions, their contexts and current
shortcuts, with Default / Modified labels. Search accepts an action name, context
or combination. Record captures the next key press without invoking its existing
action; modifiers alone are ignored. Esc cancels recording and keeps Settings
open. Confirm assigns the captured combination; it does not save automatically.
An action can have several combinations. Clicking a combination disables it;
Reset restores that action/context, and Reset all removes the user overrides.

Assigning a combination already used in the same context offers **Reassign
shortcut** or Cancel. Reassign removes the old assignment in that context.
Resetting an action also asks before taking a default key from another modified
action. Overlapping broader/global contexts produce warnings; ReadMode and
Editor can use the same combination independently. Ctrl+Alt on Windows warns
about AltGr, and commonly reserved OS/window-manager combinations have a soft
warning. Okilum cannot capture a combination the OS never delivers.

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
