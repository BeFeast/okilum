#!/usr/bin/env python3
"""Native #931 in the app: a long mixed Hebrew paragraph wraps inside the text
column in Live Preview as it does in Source. Run on an exclusive X11 display.
Usage: DISPLAY=:1034 python3 scripts/check-lp-rtl-wrap-native.py OKILUM_BINARY WORK_DIR LABEL
Per window width (1366, 683) the note is opened in Source and in Live Preview.
Readout: ink rows of the note column between a Latin marker line above and a
Latin marker line below the paragraph. Source is the control: it wraps the
paragraph into at least four rows inside the column, which gives the column's
edges (Source uses a monospace font, so its row count is not Live Preview's).
Live Preview must wrap the paragraph into at least four rows (the bug drew two
or three, the first running past the column edge), every row inside the edges.
Exit 0: all hold; 1: a check failed; 2: the run proves nothing.
"""
import os, shutil, subprocess, sys, time
from pathlib import Path

binary, work, label = sys.argv[1], Path(sys.argv[2]), sys.argv[3]
HEBREW = 'שלום עולם זהו טקסט ארוך בעברית שנועד להתעטף בשורות רבות בחלון הצר ואנחנו בודקים '
PARAGRAPH = 'abc 123 ' + HEBREW * 5 + 'xyz end'
NOTE = f'TOPMARK\n\n{PARAGRAPH}\n\nENDMARK **bold**\n'


def xd(env, *args):
    subprocess.run(['xdotool', *map(str, args)], env=env, check=True)
    time.sleep(.3)


def ink_rows(path, x0, x1):
    """[top, bottom, left, right] of each ink band between x0 and x1."""
    w, h = x1 - x0, 630
    out = subprocess.run(['convert', str(path), '-crop', f'{w}x{h}+{x0}+95', '+repage',
                          '-depth', '8', 'txt:-'], capture_output=True, text=True, check=True).stdout
    rows = {}
    for line in out.splitlines()[1:]:
        xy, rest = line.split(':', 1)
        px, py = map(int, xy.split(','))
        r, g, b = (int(v) for v in rest.split('(')[1].split(')')[0].split(',')[:3])
        if abs((r + g + b) / 3 - background) > 60:
            lo, hi = rows.get(py + 95, (10**6, -1))
            rows[py + 95] = (min(lo, px + x0), max(hi, px + x0))
    bands, cur = [], None
    for y in sorted(rows):
        if cur and y - cur[1] <= 2:
            cur = [cur[0], y, min(cur[2], rows[y][0]), max(cur[3], rows[y][1])]
        else:
            if cur and cur[1] - cur[0] >= 5:
                bands.append(cur)
            cur = [y, y, rows[y][0], rows[y][1]]
    if cur and cur[1] - cur[0] >= 5:
        bands.append(cur)
    return bands


def run(width, mode):
    app = work / label / f'{width}-{mode}'
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
            print(label, width, mode, 'INVALID: no window', flush=True)
            return None
        win = found[-1]
        xd(env, 'windowmove', win, 0, 0, 'windowsize', win, width, 740, 'windowfocus', win)
        time.sleep(6)
        xd(env, 'key', 'ctrl+shift+e' if mode == 'live' else 'ctrl+e')
        time.sleep(3)
        xd(env, 'key', 'ctrl+End')  # caret on ENDMARK, away from the paragraph
        time.sleep(1.5)
        shot = work / f'{label}-{width}-{mode}.png'
        subprocess.run(['import', '-window', 'root', str(shot)], env=env, check=True)
        return shot
    finally:
        proc.terminate()
        proc.wait(timeout=10)


def background_of(path, x, y):
    out = subprocess.run(['convert', str(path), '-crop', f'1x1+{x}+{y}', '-depth', '8', 'txt:-'],
                         capture_output=True, text=True, check=True).stdout.splitlines()[1]
    return sum(int(v) for v in out.split('(')[1].split(')')[0].split(',')[:3]) / 3


failed = invalid = False
for width in (1366, 683):
    shots = {mode: run(width, mode) for mode in ('source', 'live')}
    if not all(shots.values()):
        invalid = True
        continue
    # The note column: the editor area right of any sidebar. The Source TOPMARK
    # row's left edge is the text start; the column ends at the window edge.
    background = background_of(shots['source'], width - 30, 400)
    found = {}
    for mode, shot in shots.items():
        # Right of the sidebar at 1366 (it ends near x=280), left of the outline panel.
        bands = ink_rows(shot, 290 if width > 1000 else 8, 1080 if width > 1000 else width - 4)
        found[mode] = bands
    print(label, width, {m: [b[:4] for b in v] for m, v in found.items()}, flush=True)
    # Paragraph rows sit between the first band (TOPMARK) and the ENDMARK band.
    def paragraph(bands):
        if len(bands) < 3:
            return None
        return bands[1:-1]
    src, live = paragraph(found['source']), paragraph(found['live'])
    if not src or len(src) < 4:
        print(label, width, 'INVALID: Source did not wrap the paragraph into rows', flush=True)
        invalid = True
        continue
    left, right = min(b[2] for b in src), max(b[3] for b in src)
    checks = {'wraps_into_rows': live is not None and len(live) >= 4,
              'rows_inside_column': live is not None and all(left - 8 <= b[2] and b[3] <= right + 8 for b in live)}
    print(label, width, 'source rows', len(src), 'live rows', len(live or []), checks, flush=True)
    failed |= not all(checks.values())
sys.exit(2 if invalid else 1 if failed else 0)
