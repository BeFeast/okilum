#!/usr/bin/env python3
"""Frames for #1034's note gutter: Reader and Live Preview, light and dark, at
1366x768 and in a narrow 620x600 window. Live Preview frames have the caret at
the end of the H2, so its `##` is revealed. Run on an exclusive X11 display
(never maestro); compare the frames of two builds side by side.
Usage: DISPLAY=:1034 python3 scripts/capture-note-gutter-frames.py OKILUM_BINARY OUT_DIR LABEL
"""
import json, os, shutil, subprocess, sys, time
from pathlib import Path

binary, out, label = sys.argv[1], Path(sys.argv[2]), sys.argv[3]
# Short lines that never wrap, so four Downs from the top always reach the H2.
NOTE = ('# Big heading\n\nA short body line.\n\n## Medium heading\n\nMore body text.\n\n'
        '### Small heading\n\n- a list item\n- another\n')


def xd(env, *args):
    subprocess.run(['xdotool', *args], env=env, check=True)
    time.sleep(.3)


def frames(theme, width, height):
    app = out / label / f'{theme}-{width}'
    shutil.rmtree(app, ignore_errors=True)
    (app / 'vault').mkdir(parents=True)
    (app / 'vault' / 'gutter.md').write_text(NOTE)
    home = app / 'home'
    (home / '.config' / 'okilum').mkdir(parents=True)
    (home / '.config' / 'okilum' / 'appearance.json').write_text(json.dumps({'appearance': theme}))
    env = dict(os.environ, WAYLAND_DISPLAY='', HOME=str(home), XDG_CONFIG_HOME=str(home / '.config'),
               XDG_DATA_HOME=str(home / 'share'), XDG_CACHE_HOME=str(home / 'cache'),
               XDG_STATE_HOME=str(home / 'state'))
    proc = subprocess.Popen([binary, '--vault', str(app / 'vault'), '--index-dir', str(app / 'index'),
                             '--note', 'gutter.md'], env=env, stdout=open(app / 'app.log', 'w'),
                            stderr=subprocess.STDOUT)
    try:
        for _ in range(40):
            time.sleep(.25)
            found = subprocess.run(['xdotool', 'search', '--onlyvisible', '--pid', str(proc.pid)],
                                   env=env, capture_output=True, text=True).stdout.split()
            if found:
                break
        else:
            print(label, theme, width, 'no window', flush=True)
            return
        win = found[-1]
        xd(env, 'windowmove', win, '0', '0', 'windowsize', win, str(width), str(height),
           'windowfocus', win)
        time.sleep(3)
        shot = lambda name: subprocess.run(
            ['import', '-window', win, str(out / f'{label}-{name}-{theme}-{width}.png')],
            env=env, check=True)
        shot('reader')
        xd(env, 'key', 'ctrl+shift+e')  # Live Preview
        time.sleep(1.5)
        xd(env, 'key', 'ctrl+Home')
        for _ in range(4):
            xd(env, 'key', 'Down')
        xd(env, 'key', 'End')  # caret at the end of `## Medium heading`
        time.sleep(1)
        shot('live')
        print(label, theme, width, 'ok', flush=True)
    finally:
        proc.terminate()
        proc.wait(timeout=10)


out.mkdir(parents=True, exist_ok=True)
for theme in ['light', 'dark']:
    frames(theme, 1366, 768)
    frames(theme, 620, 600)
