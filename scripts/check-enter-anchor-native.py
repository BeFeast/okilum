#!/usr/bin/env python3
"""Native #1096: Enter near the top of a scrolled Live Preview keeps the rows
above the caret where they were. Run on an exclusive X11 display (never maestro).
Usage: DISPLAY=:1034 python3 scripts/check-enter-anchor-native.py OKILUM_BINARY WORK_DIR LABEL
Steps (#1096): a long note of list items with nested quotes, Live Preview,
Ctrl+Home, PageDown x4, click a quote row near the top of the viewport, End,
screenshot, Enter, screenshot. Readout: ink bands of the note column. The bands
above the clicked row must keep their tops (within 2 px); the new line appears
below. The click row itself is located by its band. Exit 0: unchanged; 1: the
rows above moved; 2: the run proves nothing (no window or bands, or nothing
changed at all after Enter, i.e. the key did not reach the editor).
"""
import os, shutil, subprocess, sys, time
from pathlib import Path

binary, work, label = sys.argv[1], Path(sys.argv[2]), sys.argv[3]
# A binary built together with an example carries GPUI test-support
# and fails falsely: refuse it as an INVALID run (#1043).
sys.path.insert(0, str(Path(__file__).resolve().parent))
from native_binary_guard import require_app_binary  # noqa: E402

require_app_binary(binary)
UNIT = ('- item **bold** [label](https://example.com/x)\n  > quote *q* [[Target|alias]]\n\n'
        'para **p**\n\n')
NOTE = '# Long nested note\n\n' + UNIT * 180
CROP = (284, 96, 776, 600)


def xd(env, *args):
    subprocess.run(['xdotool', *args], env=env, check=True)
    time.sleep(.3)


def bands(path):
    x0, y0, w, h = CROP
    out = subprocess.run(['convert', str(path), '-crop', f'{w}x{h}+{x0}+{y0}', '+repage',
                          '-colorspace', 'Gray', '-threshold', '55%', '-negate',
                          '-scale', f'1x{h}!', '-depth', '16', 'txt:-'],
                         capture_output=True, text=True, check=True).stdout
    rows = []
    for line in out.splitlines()[1:]:
        xy, rest = line.split(':', 1)
        if int(rest.split('(')[1].split(')')[0].split(',')[0]) > 0:
            rows.append(y0 + int(xy.split(',')[1]))
    found, cur = [], None
    for y in rows:
        if cur and y - cur[1] <= 1:
            cur[1] = y
        else:
            if cur:
                found.append(tuple(cur))
            cur = [y, y]
    if cur:
        found.append(tuple(cur))
    return [b for b in found if b[1] - b[0] >= 4]


app = work / label
shutil.rmtree(app, ignore_errors=True)
(app / 'vault').mkdir(parents=True)
(app / 'vault' / 'long.md').write_text(NOTE)
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
    else:
        print(label, 'INVALID run: no window', flush=True)
        sys.exit(2)
    win = found[-1]
    xd(env, 'windowmove', win, '0', '0', 'windowsize', win, '1366', '768', 'windowfocus', win)
    time.sleep(6)
    xd(env, 'key', 'ctrl+shift+e')
    time.sleep(3)
    xd(env, 'key', 'ctrl+Home')
    for _ in range(4):
        xd(env, 'key', 'Next')
    time.sleep(1.5)
    first = work / f'{label}-0-scrolled.png'
    subprocess.run(['import', '-window', 'root', str(first)], env=env, check=True)
    top_bands = bands(first)
    # A quote row near the top: the second or third band (the indented ones
    # are quotes), clicked at x 410 as in the report.
    target = next((b for b in top_bands[1:6] if b[0] > CROP[1] + 20), None)
    if not target:
        print(label, 'INVALID run: no rows', top_bands[:6], flush=True)
        sys.exit(2)
    xd(env, 'mousemove', '410', str((target[0] + target[1]) // 2), 'click', '1')
    time.sleep(1)
    xd(env, 'key', 'End')
    time.sleep(1.5)
    before = work / f'{label}-1-before.png'
    subprocess.run(['import', '-window', 'root', str(before)], env=env, check=True)
    xd(env, 'key', 'Return')
    time.sleep(2)
    after = work / f'{label}-2-after.png'
    subprocess.run(['import', '-window', 'root', str(after)], env=env, check=True)
    b, a = bands(before), bands(after)
    above = [band for band in b if band[1] < target[0] - 2]
    moved = [(x[0], y[0]) for x, y in zip(above, a) if abs(x[0] - y[0]) > 2]
    changed = b != a
    print(label, {'clicked_row': target, 'above': len(above), 'moved_above': moved[:4],
                  'frame_changed': changed}, flush=True)
    if not changed or not above:
        print(label, 'INVALID run: Enter changed nothing or no rows above', flush=True)
        sys.exit(2)
    sys.exit(0 if not moved else 1)
finally:
    proc.terminate()
    proc.wait(timeout=10)
