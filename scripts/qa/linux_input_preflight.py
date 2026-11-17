#!/usr/bin/env python3
"""Prove that native Linux QA pointer input works before a session touches a candidate.

Run it on the QA host inside the graphical user's environment (for muninn:
``ssh muninn python3 - < scripts/qa/linux_input_preflight.py``). It never repairs
the host: a missing daemon, a stale socket or a permission problem is reported as
``BLOCKED_HARNESS`` with the reason, and the QA session must stop there.

Checks, in order:

1. ``ydotool`` is installed.
2. The socket the ydotool client will use is resolved exactly like ydotool 1.0.x
   does it: ``$YDOTOOL_SOCKET``, else ``$XDG_RUNTIME_DIR/.ydotool_socket``, else
   ``/tmp/.ydotool_socket``.
3. That path is a socket a ``ydotoold`` is listening on (a datagram ``connect``
   succeeds). ``ECONNREFUSED`` means a stale socket left by a dead daemon.
4. Positive control: the pointer position is read from the compositor, ydotool
   moves the pointer by a few pixels, the position must change, and the pointer is
   moved back. Nothing is clicked or typed.

Exit codes: 0 PASS, 2 BLOCKED_HARNESS, 64 usage error. The last stdout line is
``INPUT_PREFLIGHT PASS ...`` or ``INPUT_PREFLIGHT BLOCKED_HARNESS reason=...``.
See docs/linux-native-input.md.
"""
import argparse
import errno
import json
import os
import shutil
import socket
import stat
import subprocess
import sys
import time

BLOCKED = 2
DEFAULT_SOCKET_NAME = ".ydotool_socket"
FALLBACK_SOCKET = "/tmp/.ydotool_socket"
UINPUT = "/dev/uinput"
USER_UNIT = "ydotool.service"
STEP = 40


class Blocked(Exception):
    def __init__(self, reason, message, **details):
        super().__init__(message)
        self.reason = reason
        self.message = message
        self.details = details


def resolve_socket(env):
    """Return (path, source) the ydotool 1.0.x client would connect to."""
    explicit = env.get("YDOTOOL_SOCKET")
    if explicit:
        return explicit, "YDOTOOL_SOCKET"
    runtime = env.get("XDG_RUNTIME_DIR")
    if runtime:
        return os.path.join(runtime, DEFAULT_SOCKET_NAME), "XDG_RUNTIME_DIR"
    return FALLBACK_SOCKET, "default"


def candidate_sockets(env):
    paths = []
    for path in (resolve_socket(env)[0],
                 os.path.join(env.get("XDG_RUNTIME_DIR") or f"/run/user/{os.getuid()}", DEFAULT_SOCKET_NAME),
                 FALLBACK_SOCKET):
        if path not in paths:
            paths.append(path)
    return paths


def probe_socket(path):
    """Return None when a daemon accepts datagrams on path, else (reason, message)."""
    try:
        info = os.stat(path)
    except FileNotFoundError:
        return "no-socket", f"no ydotoold socket at {path}: the daemon is not running"
    except PermissionError:
        return "socket-permission", f"cannot stat {path}: permission denied"
    if not stat.S_ISSOCK(info.st_mode):
        return "not-a-socket", f"{path} exists but is not a socket"
    client = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
    try:
        client.connect(path)
    except OSError as error:
        if error.errno == errno.ECONNREFUSED:
            return ("stale-socket", f"{path} refuses connections: stale socket left by a ydotoold "
                    "that is no longer running")
        if error.errno in (errno.EACCES, errno.EPERM):
            return ("socket-permission", f"{path} is owned by uid {info.st_uid} mode "
                    f"{stat.S_IMODE(info.st_mode):04o}; uid {os.getuid()} may not connect")
        return "socket-error", f"{path}: {error.strerror or error}"
    finally:
        client.close()
    return None


def run(command, timeout=10):
    return subprocess.run(command, capture_output=True, text=True, timeout=timeout)


def diagnose(env):
    """Collect read-only facts that explain why the daemon is not reachable."""
    facts = {"uid": os.getuid(), "uinput_rw": os.access(UINPUT, os.R_OK | os.W_OK)
             if os.path.exists(UINPUT) else None}
    if shutil.which("pgrep"):
        found = run(["pgrep", "-a", "ydotoold"])
        facts["daemons"] = [line for line in found.stdout.splitlines() if line.strip()]
    if shutil.which("systemctl"):
        state = run(["systemctl", "--user", "is-active", USER_UNIT])
        facts["user_unit"] = state.stdout.strip() or state.stderr.strip()
    live = [path for path in candidate_sockets(env) if probe_socket(path) is None]
    facts["listening_sockets"] = live
    return facts


def gnome_pointer():
    output = run(["muninn-eval", "global.get_pointer()"]).stdout.strip()
    x, y = json.loads(output)[:2]
    return int(x), int(y)


def hyprland_pointer():
    output = run(["hyprctl", "cursorpos"]).stdout.strip()
    x, y = (part.strip() for part in output.split(","))
    return int(float(x)), int(float(y))


def select_probe(choice, env):
    if choice == "gnome" or (choice == "auto" and shutil.which("muninn-eval")):
        return "gnome", gnome_pointer
    if choice == "hyprland" or (choice == "auto" and env.get("HYPRLAND_INSTANCE_SIGNATURE")
                                and shutil.which("hyprctl")):
        return "hyprland", hyprland_pointer
    return None, None


def move(dx):
    result = run(["ydotool", "mousemove", "-x", str(dx), "-y", "0"])
    if result.returncode != 0:
        raise Blocked("ydotool-failed", f"ydotool mousemove failed: {result.stderr.strip() or result.returncode}")


def read(read_pointer):
    try:
        return tuple(read_pointer())
    except Exception as error:  # noqa: BLE001 - any probe failure blocks the harness
        raise Blocked("probe-failed", f"cannot read the pointer position: {error}") from error


def positive_control(read_pointer, settle=0.5):
    before = read(read_pointer)
    dx = -STEP if before[0] > 4 * STEP else STEP
    move(dx)
    time.sleep(settle)
    try:
        after = read(read_pointer)
    finally:
        move(-dx)
    if after == before:
        raise Blocked("input-not-delivered", "ydotoold accepted the move but the compositor pointer did not "
                      f"change (stayed at {before}); the virtual device is not reaching the session",
                      before=before, after=after)
    return before, after


def preflight(env, probe="auto", socket_only=False, settle=0.5):
    if not shutil.which("ydotool"):
        raise Blocked("ydotool-missing", "ydotool is not installed or not on PATH")
    path, source = resolve_socket(env)
    problem = probe_socket(path)
    if problem:
        reason, message = problem
        facts = diagnose(env)
        others = [other for other in facts.get("listening_sockets", []) if other != path]
        if others:
            reason = "socket-path-mismatch"
            message = (f"client resolves {path} (from {source}) but ydotoold listens on {others[0]}; "
                       f"export YDOTOOL_SOCKET={others[0]}")
        elif facts.get("uinput_rw") is False and not facts.get("daemons"):
            message += (f"; uid {facts['uid']} cannot open {UINPUT}, so a user-level ydotoold "
                        f"({USER_UNIT}: {facts.get('user_unit', 'unknown')}) cannot start")
        raise Blocked(reason, message, socket=path, socket_source=source, **facts)
    result = {"socket": path, "socket_source": source}
    if socket_only:
        result["control"] = "skipped"
        return result
    name, reader = select_probe(probe, env)
    if not reader:
        raise Blocked("no-pointer-probe", "no compositor pointer probe (muninn-eval or hyprctl); "
                      "pass --socket-only to check only the daemon", socket=path)
    before, after = positive_control(reader, settle)
    result.update(control=name, before=list(before), after=list(after))
    return result


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--probe", choices=("auto", "gnome", "hyprland"), default="auto",
                        help="how to read the pointer position for the positive control")
    parser.add_argument("--socket-only", action="store_true",
                        help="check the daemon socket only; the positive control is NOT proven")
    parser.add_argument("--json", action="store_true", help="print the result as one JSON object")
    try:
        args = parser.parse_args(argv)
    except SystemExit as exit_:
        return 64 if exit_.code else 0
    env = dict(os.environ)
    try:
        result = preflight(env, args.probe, args.socket_only)
    except Blocked as blocked:
        payload = {"status": "BLOCKED_HARNESS", "reason": blocked.reason, "message": blocked.message,
                   **blocked.details}
        if args.json:
            print(json.dumps(payload, sort_keys=True))
        else:
            for key, value in sorted(blocked.details.items()):
                print(f"  {key}: {value}", file=sys.stderr)
            print(f"INPUT_PREFLIGHT BLOCKED_HARNESS reason={blocked.reason}: {blocked.message}")
        return BLOCKED
    payload = {"status": "PASS", **result}
    if args.json:
        print(json.dumps(payload, sort_keys=True))
    else:
        moved = (f" moved={tuple(result['before'])}->{tuple(result['after'])}" if "before" in result
                 else " (positive control NOT proven)")
        print(f"INPUT_PREFLIGHT PASS socket={result['socket']} control={result['control']}{moved}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
