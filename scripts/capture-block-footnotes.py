#!/usr/bin/env python3
"""Native #1067 (block footnotes): capture Live Preview of a note whose table
cell has a footnote reference. Expected: the cell shows the note-wide number
(superscript 3: the inline footnote is 1, [^a] is 2), not a raw `[^b]`, and no
footnote list appears under the table. The raw definition rows at the bottom
prove the frame is Live Preview. Inspect the PNG.
Usage: DISPLAY=:1034 python3 scripts/capture-block-footnotes.py OKILUM_BINARY WORK_DIR LABEL
"""
import os, shutil, subprocess, sys, time
from pathlib import Path

binary, work, label = sys.argv[1], Path(sys.argv[2]), sys.argv[3]
NOTE = ('Intro^[first] and [^a].\n\n| Name | Note |\n|---|---|\n| one | see [^b] |\n\n'
        'Text after.\n\n[^a]: Alpha.\n[^b]: Beta.\n')
app = work / label
shutil.rmtree(app, ignore_errors=True)
(app / 'vault').mkdir(parents=True)
(app / 'vault' / 'notes.md').write_text(NOTE)
home = app / 'home'
env = dict(os.environ, WAYLAND_DISPLAY='', HOME=str(home), XDG_CONFIG_HOME=str(home / '.config'),
           XDG_DATA_HOME=str(home / 'share'), XDG_CACHE_HOME=str(home / 'cache'),
           XDG_STATE_HOME=str(home / 'state'))
proc = subprocess.Popen([binary, '--vault', str(app / 'vault'), '--index-dir', str(app / 'index'),
                         '--note', 'notes.md'], env=env, stdout=open(app / 'app.log', 'w'),
                        stderr=subprocess.STDOUT)
try:
    for _ in range(40):
        time.sleep(.25)
        found = subprocess.run(['xdotool', 'search', '--onlyvisible', '--pid', str(proc.pid)],
                               env=env, capture_output=True, text=True).stdout.split()
        if found:
            break
    else:
        sys.exit(2)
    win = found[-1]
    subprocess.run(['xdotool', 'windowmove', win, '0', '0', 'windowsize', win, '1366', '768',
                    'windowfocus', win], env=env, check=True)
    time.sleep(8)  # loaded hosts: let the Reader settle before the shortcut
    subprocess.run(['xdotool', 'key', 'ctrl+shift+e'], env=env, check=True)
    time.sleep(4)
    subprocess.run(['xdotool', 'key', 'ctrl+Home'], env=env, check=True)
    time.sleep(1.5)
    out = work / f'{label}-block-footnotes.png'
    subprocess.run(['import', '-window', 'root', '-crop', '776x360+284+96', '+repage', str(out)],
                   env=env, check=True)
    print(label, out, flush=True)
finally:
    proc.terminate()
    proc.wait(timeout=10)
