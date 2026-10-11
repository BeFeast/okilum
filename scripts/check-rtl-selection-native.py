#!/usr/bin/env python3
"""Native #1208: Hebrew text selects and links open in the Reader, Live Preview
and Source. Run on an exclusive X11 display (never maestro).
Usage: DISPLAY=:1034 python3 scripts/check-rtl-selection-native.py OKILUM_BINARY WORK_DIR LABEL
Note: a Latin paragraph, `שלום עולם זה טקסט`, `קישור אל [[target|היעד]] כאן`.
Per mode (each scenario with a link runs in a fresh launch, so no selection is
left over):
- double-click a Latin word (positive control: a whole Latin word copies);
- double-click the middle of the Hebrew row (expected: one whole Hebrew word);
- drag across the Hebrew row, end to end (expected: the Hebrew text);
- click the link's text (Reader) or Ctrl+click it (editor): the target opens.
Exit 0: all hold; 1: a check failed; 2: the run proves nothing.
"""
import os, shutil, subprocess, sys, time
from pathlib import Path

binary, work, label = sys.argv[1], Path(sys.argv[2]), sys.argv[3]
LATIN = ('A long Latin paragraph that wraps across the column width so that its first row '
         'shows where the wrap edge is, again and again and again and again and again.')
HEBREW = 'שלום עולם זה טקסט'
LINKED = 'קישור אל [[target|היעד]] כאן'
NOTE = f'{LATIN}\n\n{HEBREW}\n\n{LINKED}\n\nshort latin\n'
CROP = (284, 96, 776, 400)


def xd(env, *args):
    subprocess.run(['xdotool', *args], env=env, check=True)
    time.sleep(.3)


def pixels(path):
    x0, y0, w, h = CROP
    out = subprocess.run(['convert', str(path), '-crop', f'{w}x{h}+{x0}+{y0}', '+repage',
                          '-depth', '8', 'txt:-'], capture_output=True, text=True, check=True).stdout
    for line in out.splitlines()[1:]:
        xy, rest = line.split(':', 1)
        px, py = map(int, xy.split(','))
        r, g, b = (int(v) for v in rest.split('(')[1].split(')')[0].split(',')[:3])
        yield x0 + px, y0 + py, r, g, b


def bands(path):
    rows = {}
    for x, y, r, g, b in pixels(path):
        if (r + g + b) / 3 < 140 and x > CROP[0] + 3:
            lo, hi = rows.get(y, (10**6, -1))
            rows[y] = (min(lo, x), max(hi, x))
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
    return out


def link_centre(path, band):
    blue = [(x, y) for x, y, r, g, b in pixels(path)
            if band[0] - 2 <= y <= band[1] + 2 and b > 140 and b > r + 50]
    if not blue:
        return None
    xs, ys = sorted(p[0] for p in blue), sorted(p[1] for p in blue)
    return xs[len(xs) // 2], ys[len(ys) // 2]


def copy(env, select_all=False):
    subprocess.run(['xclip', '-i', '-selection', 'clipboard'], env=env, check=True,
                   input='<stale>', text=True)
    if select_all:
        xd(env, 'key', 'ctrl+a')
    xd(env, 'key', 'ctrl+c')
    time.sleep(.5)
    return subprocess.run(['xclip', '-o', '-selection', 'clipboard'], env=env,
                          capture_output=True, text=True).stdout


def launch(mode, tag):
    app = work / label / f'{mode}-{tag}'
    shutil.rmtree(app, ignore_errors=True)
    (app / 'vault').mkdir(parents=True)
    (app / 'vault' / 'rtl.md').write_text(NOTE)
    (app / 'vault' / 'target.md').write_text('# Target page\n\nArrived.\n')
    home = app / 'home'
    env = dict(os.environ, WAYLAND_DISPLAY='', HOME=str(home), XDG_CONFIG_HOME=str(home / '.config'),
               XDG_DATA_HOME=str(home / 'share'), XDG_CACHE_HOME=str(home / 'cache'),
               XDG_STATE_HOME=str(home / 'state'))
    proc = subprocess.Popen([binary, '--vault', str(app / 'vault'), '--index-dir', str(app / 'index'),
                             '--note', 'rtl.md'], env=env, stdout=open(app / 'app.log', 'w'),
                            stderr=subprocess.STDOUT)
    for _ in range(40):
        time.sleep(.25)
        found = subprocess.run(['xdotool', 'search', '--onlyvisible', '--pid', str(proc.pid)],
                               env=env, capture_output=True, text=True).stdout.split()
        if found:
            break
    else:
        proc.terminate()
        return None, None, None
    win = found[-1]
    xd(env, 'windowmove', win, '0', '0', 'windowsize', win, '1366', '768', 'windowfocus', win)
    time.sleep(6)
    if mode != 'reader':
        xd(env, 'key', 'ctrl+shift+e' if mode == 'live' else 'ctrl+e')
        time.sleep(3)
        xd(env, 'key', 'ctrl+End')  # the caret away from both Hebrew lines
        time.sleep(1.5)
    shot = work / f'{label}-{mode}-{tag}.png'
    subprocess.run(['import', '-window', 'root', str(shot)], env=env, check=True)
    return proc, env, shot


def selections(mode):
    proc, env, shot = launch(mode, 'select')
    if not proc:
        return None
    try:
        b = bands(shot)
        if len(b) < 4:
            return {'bands': b}
        latin, hebrew = b[0], b[-3]
        mid = (hebrew[0] + hebrew[1]) // 2

        def double_click(x, y):
            xd(env, 'mousemove', str(x), str(y), 'click', '--repeat', '2', '--delay', '80', '1')
            time.sleep(.8)
            text = copy(env).strip()
            xd(env, 'mousemove', '700', str(b[-1][1] + 40), 'click', '1')  # deselect below
            return text

        result = {'latin_word': double_click(latin[2] + 30, (latin[0] + latin[1]) // 2)}
        result['hebrew_word'] = double_click((hebrew[2] + hebrew[3]) // 2, mid)
        # Drag across the whole Hebrew row, right end to left end.
        xd(env, 'mousemove', str(hebrew[3] + 2), str(mid), 'mousedown', '1')
        for x in range(hebrew[3], hebrew[2] - 6, -40):
            xd(env, 'mousemove', str(x), str(mid))
        xd(env, 'mousemove', str(hebrew[2] - 3), str(mid), 'mouseup', '1')
        time.sleep(.8)
        result['drag'] = copy(env).strip()
        return result
    finally:
        proc.terminate()
        proc.wait(timeout=10)


def link(mode):
    proc, env, shot = launch(mode, 'link')
    if not proc:
        return None
    try:
        b = bands(shot)
        if len(b) < 4:
            return {'bands': b}
        at = link_centre(shot, b[-2])
        if not at:
            return {'link_at': None}
        if mode == 'reader':
            xd(env, 'mousemove', str(at[0]), str(at[1]), 'click', '1')
        else:
            xd(env, 'mousemove', str(at[0]), str(at[1]), 'keydown', 'ctrl', 'click', '1', 'keyup', 'ctrl')
        time.sleep(3)
        after = copy(env, True)
        return {'link_at': at, 'opened': 'Target page' in after or 'Arrived' in after}
    finally:
        proc.terminate()
        proc.wait(timeout=10)


failed = invalid = False
for mode in ('reader', 'live', 'source'):
    s, l = selections(mode), link(mode)
    print(label, mode, s, l, flush=True)
    if not s or 'latin_word' not in s or not l or not l.get('link_at'):
        invalid = True
        continue
    if s['latin_word'] not in LATIN.replace(',', ' ').replace('.', ' ').split():
        print(label, mode, 'INVALID: no Latin word on double-click', flush=True)
        invalid = True
        continue
    checks = {'hebrew_word': s['hebrew_word'] in HEBREW.split(),
              'drag': s['drag'].replace('‏', '') == HEBREW,
              'link_opens': l['opened']}
    print(label, mode, checks, flush=True)
    failed |= not all(checks.values())
sys.exit(2 if invalid else 1 if failed else 0)
