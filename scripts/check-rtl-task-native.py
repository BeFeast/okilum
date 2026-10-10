#!/usr/bin/env python3
"""Native #1077: a Hebrew task in Live Preview renders a checkbox (on the right of
its right-aligned line) and a click toggles exactly the byte inside the brackets.
Run on an exclusive X11 display (never maestro).
Usage: DISPLAY=:1034 python3 scripts/check-rtl-task-native.py OKILUM_BINARY WORK_DIR LABEL
Steps: caret to the end of the note; click the English checkbox (positive control:
the leftmost ink of the first text band); click the Hebrew checkbox (the rightmost
ink of the second band); copy the whole source; undo twice; copy again.
Expected: both boxes checked, nothing else changed, and two undos restore the
note exactly. Before the fix the Hebrew `- [ ]` stayed raw, so the click only
moved the caret. Exit 1: wrong source; exit 2: the run proves nothing (no window,
bands not found, nothing copied, or the English control did not toggle).
"""
import os, shutil, subprocess, sys, time
from pathlib import Path

binary, work, label = sys.argv[1], Path(sys.argv[2]), sys.argv[3]
NOTE = '- [ ] first task\n- [ ] משימה בעברית\n\nlast line\n'
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


def edge_clusters(path):
    """Per text band: centres of its leftmost and rightmost ink clusters."""
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
    result = []
    for top, bottom in bands:
        xs = sorted({px for y in range(top, bottom + 1) for px, v in rows[y] if v < 160})

        def cluster(xs):
            first = edge = xs[0]
            for px in xs[1:]:
                if abs(px - edge) > 3:
                    break
                edge = px
            return (first + edge) // 2

        cy = y0 + (top + bottom) // 2
        result.append(((x0 + cluster(xs), cy), (x0 + cluster(xs[::-1]), cy)))
    return result


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
    bands = edge_clusters(before)
    if len(bands) < 2:
        print(label, 'INVALID run: text bands not found', bands, flush=True)
        sys.exit(2)
    english, hebrew = bands[0][0], bands[1][1]
    xd(env, 'mousemove', str(english[0]), str(english[1]), 'click', '1')
    time.sleep(1)
    xd(env, 'mousemove', str(hebrew[0]), str(hebrew[1]), 'click', '1')
    time.sleep(1)
    subprocess.run(['import', '-window', 'root', str(work / f'{label}-2-toggled.png')], env=env,
                   check=True)
    toggled = clipboard(env)
    for _ in range(2):
        xd(env, 'key', 'ctrl+z')
        time.sleep(.4)
    undone = clipboard(env)
    result = {'english': english, 'hebrew': hebrew, 'toggled': toggled, 'undone': undone}
    print(label, result, flush=True)
    if toggled == '<stale>' or undone == '<stale>':
        print(label, 'INVALID run: nothing copied', flush=True)
        sys.exit(2)
    if not toggled.startswith('- [x] first task\n'):
        print(label, 'INVALID run: the English control did not toggle', flush=True)
        sys.exit(2)
    ok = toggled.rstrip('\n') == NOTE.replace('[ ]', '[x]').rstrip('\n')
    ok &= undone.rstrip('\n') == NOTE.rstrip('\n')
    sys.exit(0 if ok else 1)
finally:
    proc.terminate()
    proc.wait(timeout=10)
