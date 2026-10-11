#!/usr/bin/env python3
"""Native #754: in a scrolled Live Preview, revealing a long link by a click, or
releasing a drag that started in it, does not move the rows above the press.
Rows below may reflow when the link's URL is revealed (accepted, owner decision
09.10: reveal per element plus a caret-row anchor); rows above must stay.
Run on an exclusive X11 display (never maestro).
Usage: DISPLAY=:1034 python3 scripts/check-lp-reveal-anchor-native.py OKILUM_BINARY WORK_DIR LABEL
Fixture (#754): 40 filler lines, TOP, a paragraph with five links whose URLs
are long plus bold text, ANCHOR, TAIL, 30 lower lines. The caret is placed on
ANCHOR so the note is scrolled and the paragraph is mid-viewport.
Readout: ink bands above the press row, before and after. Positive control: a
wheel notch between two screenshots must register as a shift.
Exit 0: all hold; 1: a check failed; 2: the run proves nothing.
"""
import os, shutil, subprocess, sys, time
from pathlib import Path

binary, work, label = sys.argv[1], Path(sys.argv[2]), sys.argv[3]
URL = 'https://example.test/' + '/'.join(f'segment-{i:02}-wide' for i in range(12))
LINKS = ' and '.join(f'[short label {i}]({URL}/{i})' for i in range(5))
PARAGRAPH = f'Paragraph with **bold text** and links: {LINKS} — end of the paragraph.'
FILL = [f'Filler line {i:02} with a few plain words.' for i in range(40)]
LOWER = [f'Lower line {i:02} with a few plain words.' for i in range(30)]
NOTE = '\n\n'.join(FILL + ['TOP plain line', PARAGRAPH, 'ANCHOR plain line', 'TAIL plain line'] + LOWER) + '\n'
ANCHOR_INDEX = len(FILL) + 2  # paragraphs before ANCHOR


def xd(env, *args):
    subprocess.run(['xdotool', *map(str, args)], env=env, check=True)
    time.sleep(.3)


def pixels(path, x0, x1, y0, y1):
    out = subprocess.run(['convert', str(path), '-crop', f'{x1 - x0}x{y1 - y0}+{x0}+{y0}', '+repage',
                          '-depth', '8', 'txt:-'], capture_output=True, text=True, check=True).stdout
    for line in out.splitlines()[1:]:
        xy, rest = line.split(':', 1)
        px, py = map(int, xy.split(','))
        r, g, b = (int(v) for v in rest.split('(')[1].split(')')[0].split(',')[:3])
        yield x0 + px, y0 + py, r, g, b


def bands(path, x0, x1, y0, y1):
    rows = {}
    for x, y, r, g, b in pixels(path, x0, x1, y0, y1):
        if (r + g + b) / 3 < 150:
            rows.setdefault(y, []).append(x)
    out, cur = [], None
    for y in sorted(rows):
        if cur and y - cur[1] <= 2:
            cur[1] = y
        else:
            if cur and cur[1] - cur[0] >= 5:
                out.append(tuple(cur))
            cur = [y, y]
    if cur and cur[1] - cur[0] >= 5:
        out.append(tuple(cur))
    return out


def link_points(path, x0, x1, y0, y1):
    """(x, y) of link-coloured pixels."""
    return [(x, y) for x, y, r, g, b in pixels(path, x0, x1, y0, y1) if b > 140 and b > r + 50]


def shot(env, path):
    subprocess.run(['import', '-window', 'root', str(path)], env=env, check=True)
    return path


def launch(width, tag):
    app = work / label / f'{width}-{tag}'
    shutil.rmtree(app, ignore_errors=True)
    (app / 'vault').mkdir(parents=True)
    (app / 'vault' / 'stability.md').write_text(NOTE)
    home = app / 'home'
    env = dict(os.environ, WAYLAND_DISPLAY='', HOME=str(home), XDG_CONFIG_HOME=str(home / '.config'),
               XDG_DATA_HOME=str(home / 'share'), XDG_CACHE_HOME=str(home / 'cache'),
               XDG_STATE_HOME=str(home / 'state'))
    proc = subprocess.Popen([binary, '--vault', str(app / 'vault'), '--index-dir', str(app / 'index'),
                             '--note', 'stability.md'], env=env, stdout=open(app / 'app.log', 'w'),
                            stderr=subprocess.STDOUT)
    for _ in range(40):
        time.sleep(.25)
        found = subprocess.run(['xdotool', 'search', '--onlyvisible', '--pid', str(proc.pid)],
                               env=env, capture_output=True, text=True).stdout.split()
        if found:
            break
    else:
        proc.terminate()
        return None, None
    win = found[-1]
    xd(env, 'windowmove', win, 0, 0, 'windowsize', win, width, 740, 'windowfocus', win)
    time.sleep(6)
    xd(env, 'key', 'ctrl+shift+e')
    time.sleep(3)
    xd(env, 'key', 'ctrl+Home')
    for _ in range(ANCHOR_INDEX * 2):  # each paragraph is followed by a blank line
        subprocess.run(['xdotool', 'key', 'Down'], env=env, check=True)
        time.sleep(.03)
    time.sleep(1.5)
    return proc, env


def run(width, action):
    proc, env = launch(width, action)
    if not proc:
        return None
    x0, x1, y0, y1 = (width // 5 if width > 1000 else 8), width - 20, 70, 730
    try:
        before = shot(env, work / f'{label}-{width}-{action}-before.png')
        links = link_points(before, x0, x1, y0, y1)
        if not links:
            return {'links': 0}
        # The middle row of link pixels: press on a link label there.
        ys = sorted({y for _, y in links})
        row_y = ys[len(ys) // 2]
        xs = sorted(x for x, y in links if abs(y - row_y) <= 3)
        press = (xs[len(xs) // 2], row_y)
        above = [b for b in bands(before, x0, x1, y0, y1) if b[1] < press[1] - 14]
        if len(above) < 3:
            return {'above': above}
        if action == 'click':
            xd(env, 'mousemove', *press, 'click', 1)
        elif action == 'drag':
            xd(env, 'mousemove', *press, 'mousedown', 1)
            for dy in range(0, 60, 12):
                xd(env, 'mousemove', press[0] + dy, press[1] + dy)
            xd(env, 'mouseup', 1)
        elif action == 'control':
            xd(env, 'mousemove', *press, 'click', 4)  # one wheel notch up: rows must move
        time.sleep(1.5)
        after = shot(env, work / f'{label}-{width}-{action}-after.png')
        moved = [b for b in bands(after, x0, x1, y0, y1) if b[1] < press[1] - 14]
        return {'press': press, 'above_before': above, 'above_after': moved,
                'stable': moved == above}
    finally:
        proc.terminate()
        proc.wait(timeout=10)


failed = invalid = False
for width in (1366, 700):
    result = {a: run(width, a) for a in ('control', 'click', 'drag')}
    for action, r in result.items():
        print(label, width, action, r, flush=True)
    if any(not r or 'stable' not in r for r in result.values()):
        invalid = True
        continue
    if result['control']['stable']:
        print(label, width, 'INVALID: a wheel notch did not register as a shift', flush=True)
        invalid = True
        continue
    checks = {'click_rows_above_still': result['click']['stable'],
              'drag_release_rows_above_still': result['drag']['stable']}
    print(label, width, checks, flush=True)
    failed |= not all(checks.values())
sys.exit(2 if invalid else 1 if failed else 0)
