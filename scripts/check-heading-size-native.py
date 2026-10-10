#!/usr/bin/env python3
"""Native #1034 (S6b): Live Preview draws headings at the Reader's sizes (H1 2x,
H2 1.5x the body) on rows tall enough to hold them; Source keeps every row at body
size. Run on an exclusive X11 display (never maestro).
Usage: DISPLAY=:1034 python3 scripts/check-heading-size-native.py OKILUM_BINARY WORK_DIR LABEL
Readout: ink bands (runs of rows holding glyphs) in the editor column, top-down:
H1, body, H2, body. Their heights are compared with the body band. The same note
in Source is the control: it must show the same four bands at body size, which
proves the crop and band detection see the text at all. Exit 1: headings are not
larger in Live Preview; exit 2: the run proves nothing (no window, bands missing).
"""
import json, os, shutil, subprocess, sys, time
from pathlib import Path

binary, work, label = sys.argv[1], Path(sys.argv[2]), sys.argv[3]
# Editor column of a 1366x768 window, below the note toolbar.
CROP = (300, 96, 760, 420)


def xd(env, *args):
    subprocess.run(['xdotool', *args], env=env, check=True)
    time.sleep(.3)


def bands(path):
    """(top, height) of each run of rows whose mean gray is below the page."""
    x, y, w, h = CROP
    out = subprocess.run(['convert', str(path), '-crop', f'{w}x{h}+{x}+{y}', '+repage',
                          '-colorspace', 'Gray', '-scale', f'1x{h}!', '-depth', '16', 'txt:-'],
                         capture_output=True, text=True, check=True).stdout
    means = []
    for line in out.splitlines()[1:]:
        means.append(int(line.split('(')[1].split(')')[0].split(',')[0]) / 65535)
    page = max(means)
    runs, start = [], None
    for row, mean in enumerate(means + [page]):
        ink = mean < page - .004
        if ink and start is None:
            start = row
        elif not ink and start is not None:
            if row - start >= 3:  # ignore caret blinks and hairlines
                runs.append((y + start, row - start))
            start = None
    return runs


def run(mode):
    app = work / label / mode
    shutil.rmtree(app, ignore_errors=True)
    (app / 'vault').mkdir(parents=True)
    (app / 'vault' / 'sizes.md').write_text(
        '# Big heading\n\nplain body text here\n\n## Medium heading\n\nplain body text here\n')
    home = app / 'home'
    (home / '.config' / 'okilum').mkdir(parents=True)
    (home / '.config' / 'okilum' / 'appearance.json').write_text(json.dumps({'appearance': 'light'}))
    env = dict(os.environ, WAYLAND_DISPLAY='', HOME=str(home), XDG_CONFIG_HOME=str(home / '.config'),
               XDG_DATA_HOME=str(home / 'share'), XDG_CACHE_HOME=str(home / 'cache'),
               XDG_STATE_HOME=str(home / 'state'))
    proc = subprocess.Popen([binary, '--vault', str(app / 'vault'), '--index-dir', str(app / 'index'),
                             '--note', 'sizes.md'], env=env, stdout=open(app / 'app.log', 'w'),
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
        # Shortcuts, not toolbar coordinates: the note toolbar is contextual (#992).
        xd(env, 'key', 'ctrl+shift+e' if mode == 'live' else 'ctrl+e')
        time.sleep(1.5)
        # Caret on the last (body) line, so no heading marker is revealed.
        xd(env, 'key', 'ctrl+End')
        time.sleep(1)
        path = work / f'{label}-{mode}.png'
        subprocess.run(['import', '-window', 'root', str(path)], env=env, check=True)
        return bands(path)
    finally:
        proc.terminate()
        proc.wait(timeout=10)


results, failed, invalid = {}, False, False
for mode in ['source', 'live']:
    found = run(mode)
    if found is None or len(found) < 4:
        print(label, mode, 'INVALID run: bands', found, flush=True)
        invalid = True
        continue
    h1, body, h2, _ = (height for _, height in found[:4])
    results[mode] = {'bands': found[:4], 'h1_ratio': round(h1 / body, 2), 'h2_ratio': round(h2 / body, 2)}
    print(label, mode, results[mode], flush=True)
if invalid:
    sys.exit(2)
# Source: headings at body size. Live Preview: H1 about 2x and H2 about 1.5x.
failed |= not (results['source']['h1_ratio'] < 1.3 and results['source']['h2_ratio'] < 1.3)
failed |= not (results['live']['h1_ratio'] > 1.6 and results['live']['h2_ratio'] > 1.25)
sys.exit(1 if failed else 0)
