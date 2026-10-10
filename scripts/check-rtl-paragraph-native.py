#!/usr/bin/env python3
"""Native #1034 (S6c): in Source and Live Preview a paragraph whose first strong
character is Hebrew is right-aligned; Latin paragraphs stay left. A click in the
middle of the Hebrew row puts the caret inside it (hit-testing follows the
alignment). Run on an exclusive X11 display (never maestro).
Usage: DISPLAY=:1034 python3 scripts/check-rtl-paragraph-native.py OKILUM_BINARY WORK_DIR LABEL
Readout: ink bands of the editor column. Band 1 is a long Latin paragraph that
wraps: its left edge is the text start and its widest row ends within a word of
the wrap edge (the control). Band 3 is the Hebrew paragraph: its right edge must sit at
the wrap edge and its left edge well right of the text start. Then a click in the
middle of the Hebrew row and `X` typed must land inside the Hebrew text, not at
either end. Exit 1: wrong alignment or caret; exit 2: the run proves nothing.
"""
import os, shutil, subprocess, sys, time
from pathlib import Path

binary, work, label = sys.argv[1], Path(sys.argv[2]), sys.argv[3]
LATIN = ('A long Latin paragraph that wraps across the editor width so that its first row '
         'shows where the wrap edge is, again and again and again.')
HEBREW = 'שלום עולם זה טקסט'
NOTE = f'{LATIN}\n\n{HEBREW}\n\nshort latin\n'
CROP = (284, 96, 776, 300)


def xd(env, *args):
    subprocess.run(['xdotool', *args], env=env, check=True)
    time.sleep(.3)


def ink(path):
    """{y: (min x, max x)} of dark pixels in the crop, in window coordinates."""
    x0, y0, w, h = CROP
    out = subprocess.run(['convert', str(path), '-crop', f'{w}x{h}+{x0}+{y0}', '+repage',
                          '-colorspace', 'Gray', '-depth', '8', 'txt:-'],
                         capture_output=True, text=True, check=True).stdout
    rows = {}
    for line in out.splitlines()[1:]:
        xy, rest = line.split(':', 1)
        px, py = map(int, xy.split(','))
        if int(rest.split('(')[1].split(')')[0].split(',')[0]) < 140 and px > 3:
            lo, hi = rows.get(y0 + py, (10**6, -1))
            rows[y0 + py] = (min(lo, x0 + px), max(hi, x0 + px))
    return rows


def bands(rows):
    out, cur = [], None
    for y in sorted(rows):
        if cur and y - cur[1] <= 2:
            cur = [cur[0], y, min(cur[2], rows[y][0]), max(cur[3], rows[y][1])]
        else:
            if cur and cur[1] - cur[0] >= 5:
                out.append(cur)
            cur = [y, y, rows[y][0], rows[y][1]]
    if cur and cur[1] - cur[0] >= 5:
        out.append(cur)
    return out  # [top, bottom, left, right]


def run(mode):
    app = work / label / mode
    shutil.rmtree(app, ignore_errors=True)
    (app / 'vault').mkdir(parents=True)
    (app / 'vault' / 'rtl.md').write_text(NOTE)
    home = app / 'home'
    env = dict(os.environ, WAYLAND_DISPLAY='', HOME=str(home), XDG_CONFIG_HOME=str(home / '.config'),
               XDG_DATA_HOME=str(home / 'share'), XDG_CACHE_HOME=str(home / 'cache'),
               XDG_STATE_HOME=str(home / 'state'))
    proc = subprocess.Popen([binary, '--vault', str(app / 'vault'), '--index-dir', str(app / 'index'),
                             '--note', 'rtl.md'], env=env, stdout=open(app / 'app.log', 'w'),
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
        # The caret stays at the start of the note, inside the Latin band, so it
        # cannot read as a band of its own.
        shot = work / f'{label}-{mode}.png'
        subprocess.run(['import', '-window', 'root', str(shot)], env=env, check=True)
        found_bands = bands(ink(shot))
        if len(found_bands) < 3:
            return {'bands': found_bands}
        latin, hebrew = found_bands[0], found_bands[-2]
        # Word wrap ends Latin rows up to a word short of the wrap edge: take the
        # widest row of the Latin paragraph.
        latin_edge = max(band[3] for band in found_bands[:-2])
        # Click the middle of the Hebrew row and type a marker.
        xd(env, 'mousemove', str((hebrew[2] + hebrew[3]) // 2), str((hebrew[0] + hebrew[1]) // 2),
           'click', '1')
        time.sleep(1)
        xd(env, 'type', 'X')
        time.sleep(.5)
        subprocess.run(['xclip', '-i', '-selection', 'clipboard'], env=env, check=True,
                       input='<stale>', text=True)
        xd(env, 'key', 'ctrl+a')
        xd(env, 'key', 'ctrl+c')
        time.sleep(.5)
        copied = subprocess.run(['xclip', '-o', '-selection', 'clipboard'], env=env,
                                capture_output=True, text=True).stdout
        line = next((l for l in copied.split('\n') if 'X' in l), '')
        return {'latin': latin, 'latin_edge': latin_edge, 'hebrew': hebrew, 'x_line': line}
    finally:
        proc.terminate()
        proc.wait(timeout=10)


failed = invalid = False
for mode in ['source', 'live']:
    r = run(mode)
    print(label, mode, r, flush=True)
    if not r or 'hebrew' not in r:
        invalid = True
        continue
    text_left, latin_edge = r['latin'][2], r['latin_edge']
    _, _, heb_left, heb_right = r['hebrew']
    # Flush right: at or past every Latin row's end, within one word of it.
    right_aligned = (latin_edge - 2 <= heb_right <= latin_edge + 80
                     and heb_left > text_left + 150)
    line = r['x_line']
    inside = line.replace('X', '') == HEBREW and not line.startswith('X') and not line.endswith('X')
    print(label, mode, {'right_aligned': right_aligned, 'click_inside': inside}, flush=True)
    failed |= not (right_aligned and inside)
sys.exit(2 if invalid else 1 if failed else 0)
