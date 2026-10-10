#!/usr/bin/env python3
"""Native #1093: typing at the very start of a BOM note writes after the BOM.
Run on an exclusive X11 display (never maestro).
Usage: DISPLAY=:1034 python3 scripts/check-bom-edit-native.py OKILUM_BINARY WORK_DIR LABEL
Steps, in Live Preview and in Source: Ctrl+Home, type `X`, Ctrl+S; read the saved
bytes. Expected: EF BB BF 58 then the original content, nothing else changed.
The saved file differing from the original at all is the positive control (the
keystroke and Save reached the file). Exit 1: wrong bytes; exit 2: the run
proves nothing (no window, or the file was not saved).
"""
import os, shutil, subprocess, sys, time
from pathlib import Path

binary, work, label = sys.argv[1], Path(sys.argv[2]), sys.argv[3]
NOTE = '﻿# BOM note\r\n\r\nFirst paragraph.\r\n'.encode()
EXPECTED = NOTE[:3] + b'X' + NOTE[3:]


def xd(env, *args):
    subprocess.run(['xdotool', *args], env=env, check=True)
    time.sleep(.3)


def run(mode):
    app = work / label / mode
    shutil.rmtree(app, ignore_errors=True)
    (app / 'vault').mkdir(parents=True)
    note = app / 'vault' / 'bom.md'
    note.write_bytes(NOTE)
    home = app / 'home'
    env = dict(os.environ, WAYLAND_DISPLAY='', HOME=str(home), XDG_CONFIG_HOME=str(home / '.config'),
               XDG_DATA_HOME=str(home / 'share'), XDG_CACHE_HOME=str(home / 'cache'),
               XDG_STATE_HOME=str(home / 'state'))
    proc = subprocess.Popen([binary, '--vault', str(app / 'vault'), '--index-dir', str(app / 'index'),
                             '--note', 'bom.md'], env=env, stdout=open(app / 'app.log', 'w'),
                            stderr=subprocess.STDOUT)
    try:
        for _ in range(40):
            time.sleep(.25)
            found = subprocess.run(['xdotool', 'search', '--onlyvisible', '--pid', str(proc.pid)],
                                   env=env, capture_output=True, text=True).stdout.split()
            if found:
                break
        else:
            return None
        win = found[-1]
        xd(env, 'windowmove', win, '0', '0', 'windowsize', win, '1366', '768', 'windowfocus', win)
        time.sleep(3)
        xd(env, 'key', 'ctrl+shift+e' if mode == 'live' else 'ctrl+e')
        time.sleep(1.5)
        xd(env, 'key', 'ctrl+Home')
        time.sleep(.5)
        xd(env, 'type', 'X')
        time.sleep(.5)
        xd(env, 'key', 'ctrl+s')
        time.sleep(1.5)
        return note.read_bytes()
    finally:
        proc.terminate()
        proc.wait(timeout=10)


results = {mode: run(mode) for mode in ('live', 'source')}
for mode, saved in results.items():
    print(label, mode, None if saved is None else saved[:12].hex(' '), flush=True)
if any(saved is None or saved == NOTE for saved in results.values()):
    print(label, 'INVALID run: no window or nothing saved', flush=True)
    sys.exit(2)
sys.exit(0 if all(saved == EXPECTED for saved in results.values()) else 1)
