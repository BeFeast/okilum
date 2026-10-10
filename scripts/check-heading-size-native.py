#!/usr/bin/env python3
"""Native #1034 (S6b): Live Preview draws headings at the Reader's sizes (H1 2x,
H2 1.5x the body) on rows tall enough to hold them; Source keeps every row at body
size. Run on an exclusive X11 display (never maestro).
Usage: DISPLAY=:1034 python3 scripts/check-heading-size-native.py OKILUM_BINARY WORK_DIR LABEL
Readout: ink bands (runs of rows holding glyphs) in the editor column, top-down:
H1, body, H2, body. Their heights are compared with the body band. The same note
in Source is the control: it must show the same four bands at body size, which
proves the crop and band detection see the text at all.
Then the caret moves to the end of the H2 by keyboard, revealing `## `. The
marker must hang in the margin: ink appears left of where the heading text
starts (it is not clipped), and the heading text itself does not move. Exit 1:
headings are not larger, or the reveal moved the text or hid the marker; exit 2:
the run proves nothing (no window, bands missing).
"""
import json, os, shutil, subprocess, sys, time
from pathlib import Path

binary, work, label = sys.argv[1], Path(sys.argv[2]), sys.argv[3]
# Note pane of a 1366x768 window from its left edge (gutter included), below
# the note toolbar.
CROP = (284, 96, 776, 420)


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
            runs.append([start, row])
            start = None
    # Descenders of a large heading can fall apart from it by a pale row or
    # two: join runs closer than 4 rows, then drop hairlines.
    merged = []
    for run in runs:
        if merged and run[0] - merged[-1][1] < 4:
            merged[-1][1] = run[1]
        else:
            merged.append(run)
    return [(y + a, b - a) for a, b in merged if b - a >= 3]


def columns(path, band):
    """Mean gray per column across one band of the editor column."""
    x, _, w, _ = CROP
    top, height = band
    out = subprocess.run(['convert', str(path), '-crop', f'{w}x{height}+{x}+{top}', '+repage',
                          '-colorspace', 'Gray', '-scale', f'{w}x1!', '-depth', '16', 'txt:-'],
                         capture_output=True, text=True, check=True).stdout
    return [int(line.split('(')[1].split(')')[0].split(',')[0]) / 65535
            for line in out.splitlines()[1:]]


def reveal(concealed, revealed, band):
    """Where the marker and the text sit before and after the reveal."""
    x = CROP[0]
    before, after = columns(concealed, band), columns(revealed, band)
    page = max(before)
    ink = [i for i, mean in enumerate(before) if mean < page - .01]
    if not ink:
        return None
    left, right = ink[0], ink[-1]
    # The caret sits right of the text end; compare the text, not the caret.
    moved = sum(1 for i in range(left, right - 3) if abs(before[i] - after[i]) > .02)
    marker = sum(1 for i in range(0, left) if after[i] < page - .01)
    return {'text_left': x + left, 'text_columns_changed': moved, 'marker_columns': marker}


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
        found = bands(path)
        if mode != 'live' or len(found) < 4:
            return found, None
        # Caret to the end of the H2 line (line 5), which reveals its `## `.
        xd(env, 'key', 'ctrl+Home')
        for _ in range(4):
            xd(env, 'key', 'Down')
        xd(env, 'key', 'End')
        time.sleep(1)
        revealed = work / f'{label}-{mode}-reveal.png'
        subprocess.run(['import', '-window', 'root', str(revealed)], env=env, check=True)
        return found, reveal(path, revealed, found[2])
    finally:
        proc.terminate()
        proc.wait(timeout=10)


results, failed, invalid = {}, False, False
for mode in ['source', 'live']:
    found, revealed = run(mode) or (None, None)
    if found is None or len(found) < 4:
        print(label, mode, 'INVALID run: bands', found, flush=True)
        invalid = True
        continue
    h1, body, h2, _ = (height for _, height in found[:4])
    results[mode] = {'bands': found[:4], 'h1_ratio': round(h1 / body, 2), 'h2_ratio': round(h2 / body, 2)}
    if mode == 'live':
        results[mode]['reveal'] = revealed
    print(label, mode, results[mode], flush=True)
if invalid:
    sys.exit(2)
# Source: headings at body size (a merged descender row can read up to ~1.3).
# Live Preview: H1 about 2x and H2 about 1.5x.
failed |= not (results['source']['h1_ratio'] < 1.4 and results['source']['h2_ratio'] < 1.4)
failed |= not (results['live']['h1_ratio'] > 1.6 and results['live']['h2_ratio'] > 1.25)
# Reveal: the `##` shows in the margin and the heading text stays put.
shown = results['live']['reveal']
failed |= not (shown and shown['marker_columns'] >= 6 and shown['text_columns_changed'] == 0)
sys.exit(1 if failed else 0)
