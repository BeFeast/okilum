#!/usr/bin/env python3
"""Native #1095: Edit -> Reader -> Edit keeps the edit's Undo history.
Run on an exclusive X11 display (never maestro).
Usage: DISPLAY=:1034 python3 scripts/check-reader-undo-native.py OKILUM_BINARY WORK_DIR LABEL
Steps: Live Preview; End; type `X`; Ctrl+S; Ctrl+E (Reader); Ctrl+E (back);
Ctrl+Z; Ctrl+S. Expected: the file is the original again. The first save
writing `X` is the positive control (typing and Save reached the file).
Exit 1: Undo did not restore; exit 2: the run proves nothing.
"""
import os, shutil, subprocess, sys, time
from pathlib import Path

binary, work, label = sys.argv[1], Path(sys.argv[2]), sys.argv[3]
NOTE = b'one **two** three\n'
EDITED = b'one **two** threeX\n'


def xd(env, *args):
    subprocess.run(['xdotool', *args], env=env, check=True)
    time.sleep(.3)


app = work / label
shutil.rmtree(app, ignore_errors=True)
(app / 'vault').mkdir(parents=True)
note = app / 'vault' / 'note.md'
note.write_bytes(NOTE)
home = app / 'home'
env = dict(os.environ, WAYLAND_DISPLAY='', HOME=str(home), XDG_CONFIG_HOME=str(home / '.config'),
           XDG_DATA_HOME=str(home / 'share'), XDG_CACHE_HOME=str(home / 'cache'),
           XDG_STATE_HOME=str(home / 'state'))
proc = subprocess.Popen([binary, '--vault', str(app / 'vault'), '--index-dir', str(app / 'index'),
                         '--note', 'note.md'], env=env, stdout=open(app / 'app.log', 'w'),
                        stderr=subprocess.STDOUT)
try:
    for _ in range(40):
        time.sleep(.25)
        found = subprocess.run(['xdotool', 'search', '--onlyvisible', '--pid', str(proc.pid)],
                               env=env, capture_output=True, text=True).stdout.split()
        if found:
            break
    else:
        print(label, 'INVALID run: no window', flush=True)
        sys.exit(2)
    win = found[-1]
    xd(env, 'windowmove', win, '0', '0', 'windowsize', win, '1366', '768', 'windowfocus', win)
    time.sleep(3)
    xd(env, 'key', 'ctrl+shift+e')  # Live Preview
    time.sleep(1.5)
    xd(env, 'key', 'ctrl+Home')
    xd(env, 'key', 'End')
    xd(env, 'type', 'X')
    xd(env, 'key', 'ctrl+s')
    time.sleep(1)
    saved = note.read_bytes()
    xd(env, 'key', 'ctrl+e')  # Reader
    time.sleep(1.5)
    xd(env, 'key', 'ctrl+e')  # back to the editor
    time.sleep(1.5)
    xd(env, 'key', 'ctrl+z')
    time.sleep(.5)
    xd(env, 'key', 'ctrl+s')
    time.sleep(1)
    after = note.read_bytes()
    print(label, {'saved': saved, 'after_undo': after}, flush=True)
    if saved != EDITED:
        print(label, 'INVALID run: the first edit did not reach the file', flush=True)
        sys.exit(2)
    sys.exit(0 if after == NOTE else 1)
finally:
    proc.terminate()
    proc.wait(timeout=10)
