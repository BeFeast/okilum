#!/usr/bin/env python3
"""Native #914 regression: Live Preview scrolls to the end of a long note by wheel
and by keyboard. Run on an exclusive X11 display (e.g. tessera-dev, never maestro).
Usage: DISPLAY=:914 python3 scripts/check-lp-scroll-native.py TESSERA_BINARY WORK_DIR
The readout is the clipboard: after scrolling, visible rows are triple-clicked and
copied from the top down until the end marker is found.
"""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time

binary, work = sys.argv[1], Path(sys.argv[2])
END = 'END-OF-NOTE-914'
lines = ['# Long note for #914', '']
for s in range(1, 41):
    lines += [f'## Section {s}', '',
              f'Paragraph {s} with **bold**, *em*, `code` and a [[Link {s}|alias]] that is long '
              'enough to wrap across the editor width in Live Preview, again and again.', '',
              f'- item {s} **b**', f'- item {s} two', '  - nested', '', f'> quote {s}', '']
lines += ['Final paragraph.', '', END]


def xd(env, *args):
    subprocess.run(['xdotool', *args], env=env, check=True)
    time.sleep(.3)


def clipboard(env):
    return subprocess.run(['xclip', '-o', '-selection', 'clipboard'], env=env,
                          capture_output=True, text=True).stdout


def run(mode, steps):
    app = work / mode
    shutil.rmtree(app, ignore_errors=True)
    (app / 'vault').mkdir(parents=True)
    (app / 'vault' / 'long.md').write_text('\n'.join(lines) + '\n')
    home = app / 'home'
    env = dict(os.environ, WAYLAND_DISPLAY='', HOME=str(home), XDG_CONFIG_HOME=str(home / '.config'),
               XDG_DATA_HOME=str(home / 'share'), XDG_CACHE_HOME=str(home / 'cache'),
               XDG_STATE_HOME=str(home / 'state'))
    proc = subprocess.Popen([binary, '--vault', str(app / 'vault'), '--index-dir', str(app / 'index'),
                             '--note', 'long.md'], env=env, stdout=open(app / 'app.log', 'w'),
                            stderr=subprocess.STDOUT)
    try:
        for _ in range(40):
            time.sleep(.25)
            found = subprocess.run(['xdotool', 'search', '--onlyvisible', '--pid', str(proc.pid)],
                                   env=env, capture_output=True, text=True).stdout.split()
            if found:
                break
        win = found[-1]
        xd(env, 'windowmove', win, '0', '0', 'windowsize', win, '1366', '768', 'windowfocus', win)
        time.sleep(3)  # an earlier click lands before the toolbar exists
        xd(env, 'mousemove', '996', '70', 'click', '1')  # Edit
        time.sleep(1.5)
        if mode == 'live':
            xd(env, 'mousemove', '968', '70', 'click', '1')  # Live Preview
            time.sleep(1.5)
        # Caret after concealed markers, near the top: the #914 trigger.
        xd(env, 'mousemove', '600', '200', 'click', '1')
        results = {}
        for name, action in steps:
            action(env)
            time.sleep(1.5)
            subprocess.run(['import', '-window', 'root', str(work / f'{mode}-{name}.png')], env=env, check=True)
            hit = False
            # Top-down: clicks on visible text rows do not move the view, while a
            # click below the last line puts the caret on the trailing empty line.
            for y in range(120, 764, 12):
                subprocess.run(['xclip', '-i', '-selection', 'clipboard'], env=env, check=True,
                               input='<stale>', text=True)
                xd(env, 'mousemove', '500', str(y), 'click', '--repeat', '3', '--delay', '60', '1')
                xd(env, 'key', 'ctrl+c')
                if END in clipboard(env):
                    hit = True
                    break
            results[name] = hit
            xd(env, 'key', 'ctrl+Home')
        return results
    finally:
        proc.terminate()
        proc.wait(timeout=10)


steps = [
    ('wheel', lambda env: xd(env, 'mousemove', '600', '400', 'click', '--repeat', '250', '--delay', '20', '5')),
    ('ctrl-end', lambda env: xd(env, 'key', 'ctrl+End')),
    ('page-down', lambda env: xd(env, 'key', '--repeat', '60', '--delay', '20', 'Next')),
]
failed = False
for mode in ['source', 'live']:
    results = run(mode, steps)
    print(mode, results, flush=True)
    failed |= not all(results.values())
sys.exit(1 if failed else 0)
