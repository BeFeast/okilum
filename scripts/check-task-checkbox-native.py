#!/usr/bin/env python3
"""Native #1034 (S5): a Live Preview task checkbox toggles on click, as one source
edit and one undo step, and the click does not move the caret.
Run on an exclusive X11 display (never maestro).
Usage: DISPLAY=:1034 python3 scripts/check-task-checkbox-native.py OKILUM_BINARY WORK_DIR LABEL
Steps: caret to the end of the note; click the first checkbox; type `Z`; click the
second checkbox; copy the whole source. Expected: first box checked, second box
cleared, `Z` at the very end (the caret stayed put). Then three undos must give the
original note back exactly: one step per toggle. The checkboxes are found on screen
as the leftmost ink of the first two text bands. Exit 1: wrong source; exit 2: the
run proves nothing (no window, boxes not found, nothing copied).
"""
import os, shutil, subprocess, sys, time
from pathlib import Path

binary, work, label = sys.argv[1], Path(sys.argv[2]), sys.argv[3]
NOTE = '- [ ] first task\n- [x] second task\n\nlast line\n'
CROP = (284, 96, 776, 300)  # note pane of a 1366x768 window, gutter included


def xd(env, *args):
    subprocess.run(['xdotool', *args], env=env, check=True)
    time.sleep(.3)


def gray_rows(path):
    x, y, w, h = CROP
    out = subprocess.run(['convert', str(path), '-crop', f'{w}x{h}+{x}+{y}', '+repage',
                          '-colorspace', 'Gray', '-depth', '8', 'txt:-'],
                         capture_output=True, text=True, check=True).stdout
    rows = {}
    for line in out.splitlines()[1:]:
        xy, rest = line.split(':', 1)
        px, py = map(int, xy.split(','))
        value = int(rest.split('(')[1].split(')')[0].split(',')[0])
        rows.setdefault(py, []).append((px, value))
    return rows


def boxes(path):
    """Centre of the leftmost ink cluster in each of the first two text bands."""
    x0, y0, _, _ = CROP
    rows = gray_rows(path)
    ink_rows = sorted(y for y, row in rows.items() if any(v < 160 for _, v in row))
    bands, start, last = [], None, None
    for y in ink_rows + [None]:
        if start is None:
            start = last = y
        elif y is not None and y - last <= 2:
            last = y
        else:
            if last - start >= 6:
                bands.append((start, last))
            start = last = y
    centres = []
    for top, bottom in bands[:2]:
        xs = sorted({px for y in range(top, bottom + 1) for px, v in rows[y] if v < 160})
        left = xs[0]
        right = left
        for px in xs:
            if px - right > 3:
                break
            right = px
        centres.append((x0 + (left + right) // 2, y0 + (top + bottom) // 2))
    return centres


def clipboard(env):
    subprocess.run(['xclip', '-i', '-selection', 'clipboard'], env=env, check=True,
                   input='<stale>', text=True)
    xd(env, 'key', 'ctrl+a')
    xd(env, 'key', 'ctrl+c')
    time.sleep(.5)
    copied = subprocess.run(['xclip', '-o', '-selection', 'clipboard'], env=env,
                            capture_output=True, text=True).stdout
    xd(env, 'key', 'ctrl+End')
    return copied


app = work / label
shutil.rmtree(app, ignore_errors=True)
(app / 'vault').mkdir(parents=True)
(app / 'vault' / 'tasks.md').write_text(NOTE)
home = app / 'home'
env = dict(os.environ, WAYLAND_DISPLAY='', HOME=str(home), XDG_CONFIG_HOME=str(home / '.config'),
           XDG_DATA_HOME=str(home / 'share'), XDG_CACHE_HOME=str(home / 'cache'),
           XDG_STATE_HOME=str(home / 'state'))
proc = subprocess.Popen([binary, '--vault', str(app / 'vault'), '--index-dir', str(app / 'index'),
                         '--note', 'tasks.md'], env=env, stdout=open(app / 'app.log', 'w'),
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
    xd(env, 'key', 'ctrl+End')  # caret at the end of the note, away from both tasks
    time.sleep(1)
    before = work / f'{label}-1-before.png'
    subprocess.run(['import', '-window', 'root', str(before)], env=env, check=True)
    centres = boxes(before)
    if len(centres) < 2:
        print(label, 'INVALID run: checkboxes not found', centres, flush=True)
        sys.exit(2)
    first, second = centres
    xd(env, 'mousemove', str(first[0]), str(first[1]), 'click', '1')
    time.sleep(1)
    xd(env, 'type', 'Z')
    time.sleep(.5)
    xd(env, 'mousemove', str(second[0]), str(second[1]), 'click', '1')
    time.sleep(1)
    subprocess.run(['import', '-window', 'root', str(work / f'{label}-2-toggled.png')], env=env,
                   check=True)
    toggled = clipboard(env)
    for _ in range(3):
        xd(env, 'key', 'ctrl+z')
        time.sleep(.4)
    undone = clipboard(env)
    result = {'boxes': centres, 'toggled': toggled, 'undone': undone}
    print(label, result, flush=True)
    if toggled == '<stale>' or undone == '<stale>':
        print(label, 'INVALID run: nothing copied', flush=True)
        sys.exit(2)
    expected = '- [x] first task\n- [ ] second task\n\nlast line\nZ'
    ok = toggled.rstrip('\n') in (expected, expected.replace('\nZ', 'Z'))
    ok &= undone.rstrip('\n') == NOTE.rstrip('\n')
    sys.exit(0 if ok else 1)
finally:
    proc.terminate()
    proc.wait(timeout=10)
