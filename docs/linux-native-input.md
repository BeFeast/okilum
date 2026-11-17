# Linux native QA input preflight

Native Linux QA sessions on a real desktop (muninn) move the pointer and type with
`ydotool`. The client talks to a `ydotoold` daemon over a Unix datagram socket, and
the daemon writes to `/dev/uinput`. When the daemon is missing, the first pointer
command fails in the middle of a session:

```
failed to connect socket `/tmp/.ydotool_socket': Connection refused
```

That is what stopped the 2026-11-01 Linux update acceptance (#2179) before any
product interaction. A QA session must prove input works **before** it touches the
candidate, and must stop with `BLOCKED_HARNESS` instead of repairing the host.

## Run the preflight

```bash
ssh muninn 'PATH=$HOME/.local/bin:$PATH python3 -' < scripts/qa/linux_input_preflight.py
```

Add `--json` for one machine-readable object. Exit codes: `0` PASS, `2`
`BLOCKED_HARNESS`, `64` usage error. The last stdout line is the verdict:

```
INPUT_PREFLIGHT PASS socket=/run/user/1000/.ydotool_socket control=gnome moved=(1229, 691)->(1189, 691)
INPUT_PREFLIGHT BLOCKED_HARNESS reason=no-socket: no ydotoold socket at ...
```

The positive control reads the pointer from the compositor (`muninn-eval
'global.get_pointer()'` on GNOME, `hyprctl cursorpos` on Hyprland), moves it 40 px
with `ydotool mousemove`, requires the position to change and moves it back. It
never clicks or types, but it does move the live pointer, so run it only while
holding the muninn GUI QA lock. `--socket-only` checks the daemon without the
control and says so in its verdict; that result does not prove input.

## Socket path

`ydotool` 1.0.x (client and daemon) resolves the socket in this order:

1. `$YDOTOOL_SOCKET`
2. `$XDG_RUNTIME_DIR/.ydotool_socket` (`/run/user/1000/.ydotool_socket` for `god`,
   also in a non-interactive `ssh muninn`)
3. `/tmp/.ydotool_socket`

The old Hyprland runbook exported `YDOTOOL_SOCKET=/tmp/.ydotool_socket` and started
the daemon by hand with `sudo -n ydotoold --socket-path=/tmp/.ydotool_socket
--socket-own=1000:1000`. Do not export that variable unless the daemon really
listens there; the preflight reports `socket-path-mismatch` with the right value
when it finds a live daemon on another candidate path.

## Blocked reasons

| reason | meaning |
|---|---|
| `ydotool-missing` | `ydotool` is not on `PATH` |
| `no-socket` | no daemon has created the socket; the message adds whether this user can open `/dev/uinput` and the state of the `ydotool.service` user unit |
| `stale-socket` | the socket file exists but nothing listens (`Connection refused`): the daemon died or was stopped, leaving the file |
| `socket-permission` | the socket belongs to another user or mode (`--socket-own` missing) |
| `socket-path-mismatch` | a daemon listens, but on a different path than the client resolves |
| `not-a-socket`, `socket-error` | the path is something else or `connect` failed otherwise |
| `no-pointer-probe` | neither `muninn-eval` nor `hyprctl` is available to read the pointer |
| `probe-failed` | the pointer probe ran but returned no position |
| `ydotool-failed` | `ydotool mousemove` exited non-zero |
| `input-not-delivered` | the daemon accepted the move but the compositor pointer did not change |

## muninn state (Ubuntu 26.04.1, GNOME 50, since 2026-11-10)

muninn no longer runs Hyprland. Its `ydotool` 1.0.4 package ships an enabled user
unit `ydotool.service` that runs `/usr/bin/ydotoold` as `god`. `/dev/uinput` is
`root:input 0660` and `god` is not in `input`, so the unit fails at every login
with `failed to open uinput device: Permission denied` and no socket exists. The
preflight reports this as `no-socket` with `uinput_rw: False` and
`user_unit: failed`.

Making the rig start a working daemon is a host change, not a repository change.
The proposal (exact commands, backup and rollback) is in #2179 and waits for an
operator decision. Until it is applied, every native session that needs `ydotool`
on muninn must stop at the preflight with `BLOCKED_HARNESS`. Left clicks alone are
available without `ydotool` through `muninn-click X Y` (Clutter virtual pointer).
