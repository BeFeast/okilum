#!/usr/bin/env python3
"""Native #1067: a press on a rendered Live Preview table reveals it at once (no
caret drawn over the rendered table while the button is held), and the caret
lands in the cell that was clicked.
Run on an exclusive X11 display (never maestro).
Usage: DISPLAY=:1034 python3 scripts/check-table-click-native.py OKILUM_BINARY WORK_DIR LABEL
Steps: Live Preview, caret at the note end (the table renders). The cell grid
is read from the rendered table's borders: rows with a long horizontal run,
columns with a long vertical run. Press on the cell holding `12` (last row,
second column), screenshot while held, release; type `Q`; copy the source;
undo; copy again.
Expected: the held frame shows no rendered table (it revealed on press), the
source has `| pear | Q12 |` and nothing else changed, and Undo restores the
note exactly. Exit 1: a step failed; exit 2: the run proves nothing (no window,
no table grid found, nothing copied).
"""
import os, shutil, subprocess, sys, time
from pathlib import Path

binary, work, label = sys.argv[1], Path(sys.argv[2]), sys.argv[3]
NOTE = ('Intro line above the table.\n\n'
        '| Name | Count | Note |\n|---|---|---|\n| apple | 3 | crisp |\n| plum | 5 | soft |\n'
        '| pear | 12 | a much longer cell that still fits |\n\n'
        'Text after the table.\n')
EXPECTED = NOTE.replace('| pear | 12 |', '| pear | Q12 |')
CROP = (284, 96, 776, 400)


def xd(env, *args):
    subprocess.run(['xdotool', *args], env=env, check=True)
    time.sleep(.3)


def shot(env, name):
    path = work / f'{label}-{name}.png'
    subprocess.run(['import', '-window', 'root', str(path)], env=env, check=True)
    return path


def ink(path):
    """{(x, y): differs from the page} over the crop, in window coordinates."""
    x0, y0, w, h = CROP
    out = subprocess.run(['convert', str(path), '-crop', f'{w}x{h}+{x0}+{y0}', '+repage',
                          '-colorspace', 'Gray', '-depth', '8', 'txt:-'],
                         capture_output=True, text=True, check=True).stdout
    values = {}
    for line in out.splitlines()[1:]:
        xy, rest = line.split(':', 1)
        px, py = map(int, xy.split(','))
        values[(px, py)] = int(rest.split('(')[1].split(')')[0].split(',')[0])
    counts = {}
    for v in values.values():
        counts[v] = counts.get(v, 0) + 1
    page = max(counts, key=counts.get)
    return {(x0 + px, y0 + py) for (px, py), v in values.items() if abs(v - page) > 6}


def runs(points, horizontal, minimum):
    """Lines (y for horizontal, x for vertical) holding a run >= minimum."""
    lines = {}
    for x, y in points:
        key, pos = (y, x) if horizontal else (x, y)
        lines.setdefault(key, set()).add(pos)
    found = []
    for key, positions in lines.items():
        best = run = 0
        last = None
        for pos in sorted(positions):
            run = run + 1 if last is not None and pos == last + 1 else 1
            best = max(best, run)
            last = pos
        if best >= minimum:
            found.append(key)
    # Merge adjacent pixels of one border.
    merged = []
    for key in sorted(found):
        if merged and key - merged[-1][-1] <= 2:
            merged[-1].append(key)
        else:
            merged.append([key])
    return [sum(group) // len(group) for group in merged]


def copy_all(env):
    subprocess.run(['xclip', '-i', '-selection', 'clipboard'], env=env, check=True,
                   input='<stale>', text=True)
    xd(env, 'key', 'ctrl+a')
    xd(env, 'key', 'ctrl+c')
    time.sleep(.5)
    copied = subprocess.run(['xclip', '-o', '-selection', 'clipboard'], env=env,
                            capture_output=True, text=True).stdout
    return copied


app = work / label
shutil.rmtree(app, ignore_errors=True)
(app / 'vault').mkdir(parents=True)
(app / 'vault' / 'table.md').write_text(NOTE)
home = app / 'home'
env = dict(os.environ, WAYLAND_DISPLAY='', HOME=str(home), XDG_CONFIG_HOME=str(home / '.config'),
           XDG_DATA_HOME=str(home / 'share'), XDG_CACHE_HOME=str(home / 'cache'),
           XDG_STATE_HOME=str(home / 'state'))
proc = subprocess.Popen([binary, '--vault', str(app / 'vault'), '--index-dir', str(app / 'index'),
                         '--note', 'table.md'], env=env, stdout=open(app / 'app.log', 'w'),
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
    xd(env, 'key', 'ctrl+End')  # caret below the table: it renders
    time.sleep(1.5)
    points = ink(shot(env, '1-rendered'))
    rows = runs(points, True, 200)
    if len(rows) < 2:
        print(label, 'INVALID run: no rendered table', rows, flush=True)
        sys.exit(2)
    top, bottom = rows[0], rows[-1]
    columns = runs({(x, y) for x, y in points if top <= y <= bottom}, False,
                   int((bottom - top) * .8))
    # Outer borders plus two inner ones: three columns. The tinted header
    # merges with the top border, so four body rows give at least four lines.
    if len(columns) < 4 or len(rows) < 4:
        print(label, 'INVALID run: table grid not found', rows, columns, flush=True)
        sys.exit(2)
    x = (columns[1] + columns[2]) // 2
    y = (rows[-2] + rows[-1]) // 2  # last row
    xd(env, 'mousemove', str(x), str(y))
    subprocess.run(['xdotool', 'mousedown', '1'], env=env, check=True)
    time.sleep(.5)
    held = shot(env, '2-held')
    subprocess.run(['xdotool', 'mouseup', '1'], env=env, check=True)
    time.sleep(1)
    revealed_on_press = len(runs(ink(held), True, 200)) == 0
    xd(env, 'type', 'Q')
    time.sleep(.5)
    shot(env, '3-typed')
    typed = copy_all(env)
    xd(env, 'key', 'ctrl+z')
    time.sleep(.5)
    undone = copy_all(env)
    result = {'click': (x, y), 'rows': rows, 'columns': columns,
              'revealed_on_press': revealed_on_press, 'typed': typed, 'undone_exact': undone == NOTE}
    print(label, result, flush=True)
    if typed == '<stale>' or undone == '<stale>':
        print(label, 'INVALID run: nothing copied', flush=True)
        sys.exit(2)
    ok = revealed_on_press and typed == EXPECTED and undone == NOTE
    sys.exit(0 if ok else 1)
finally:
    proc.terminate()
    proc.wait(timeout=10)
