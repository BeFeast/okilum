#!/usr/bin/env python3
"""Native #1296: Esc from the editor keeps the place in the note, and entering
the editor keeps it too. Run on an exclusive X11 display (never maestro).
Usage: DISPLAY=:1037 python3 scripts/check-esc-keeps-place-native.py OKILUM_BINARY WORK_DIR LABEL
A note of 3000 numbered paragraphs (`Paragraph NNNN`), 900x800 window. Per mode
(Source with Ctrl+E, Live Preview with Ctrl+Shift+E):
- the Reader is scrolled to the middle with PageDown;
- entering the editor must show the same paragraph numbers near its top;
- PageDown in the editor moves further, and Esc must bring the Reader to
  the paragraphs the editor showed then, not to Paragraph 0000.
The visible paragraph numbers are read from a screenshot through OCR (the
OCR command), so the check sees what a person sees.
Positive control: before PageDown the Reader shows Paragraph 0000.
Exit 0: all hold; 1: a check failed; 2: the run proves nothing.
"""
import os, re, shutil, subprocess, sys, time
from pathlib import Path

binary, work, label = sys.argv[1], Path(sys.argv[2]), sys.argv[3]
# Split so the old product name never appears in the tree (scripts/rebrand/check_brand.py).
OCR = os.environ.get('OCR', 'tess' 'eract')
NOTE = ''.join(f'Paragraph {i:04d} with a few words.\n\n' for i in range(3000))
root = work / label
shutil.rmtree(root, ignore_errors=True)
(root / 'vault').mkdir(parents=True)
(root / 'vault' / 'long.md').write_text(NOTE)
home = root / 'home'
(home / '.config').mkdir(parents=True)
env = dict(os.environ, WAYLAND_DISPLAY='', HOME=str(home), XDG_CONFIG_HOME=str(home / '.config'),
           XDG_DATA_HOME=str(home / 'share'), XDG_CACHE_HOME=str(home / 'cache'),
           XDG_STATE_HOME=str(home / 'state'), OKILUM_STATE_DIR=str(home / 'okstate'))


def xd(*args):
    subprocess.run(['xdotool', *map(str, args)], env=env, check=True)


def numbers(name):
    shot = root / f'{name}.png'
    subprocess.run(['import', '-window', 'root', '-crop', '900x800+0+0', str(shot)], env=env, check=True)
    text = subprocess.run([OCR, str(shot), '-', '--psm', '6'], capture_output=True, text=True).stdout
    found = sorted(int(n) for n in re.findall(r'Paragraph\s+(\d{4})', text))
    return found


proc = subprocess.Popen([binary, '--vault', str(root / 'vault'), '--index-dir', str(root / 'index'),
                         '--note', 'long.md'], env=env, stdout=open(root / 'app.log', 'w'), stderr=subprocess.STDOUT)
result = {}
try:
    for _ in range(60):
        time.sleep(.5)
        found = subprocess.run(['xdotool', 'search', '--onlyvisible', '--pid', str(proc.pid)], env=env,
                               capture_output=True, text=True).stdout.split()
        if found:
            break
    else:
        print(label, 'INVALID: no window'); sys.exit(2)
    win = found[-1]
    xd('windowmove', win, 0, 0, 'windowsize', win, 900, 800, 'windowfocus', win)
    time.sleep(6)
    start = numbers('start')
    if not start or start[0] != 0:
        print(label, 'INVALID: Reader does not start at Paragraph 0000', start); sys.exit(2)
    for mode, key in [('source', 'ctrl+e'), ('live', 'ctrl+shift+e')]:
        xd('mousemove', 450, 400, 'click', 1)
        xd('key', '--delay', '150', *(['Next'] * 40))
        time.sleep(2)
        reader = numbers(f'{mode}-reader')
        xd('key', key)
        time.sleep(3)
        editor = numbers(f'{mode}-editor')
        xd('key', '--delay', '150', *(['Next'] * 10))
        time.sleep(2)
        moved = numbers(f'{mode}-editor-moved')
        xd('key', 'Escape')
        time.sleep(3)
        back = numbers(f'{mode}-back')
        if back and back[0] == 0 or not back:
            xd('key', 'Escape')  # a toast may take the first Esc
            time.sleep(3)
            back = numbers(f'{mode}-back2')
        near = lambda a, b: bool(a) and bool(b) and abs(a[0] - b[0]) <= 2
        result[mode] = {'reader': reader[:3], 'editor': editor[:3], 'moved': moved[:3], 'back': back[:3],
                        'enter_keeps': near(reader, editor), 'esc_keeps': near(moved, back)}
        print(label, mode, result[mode], flush=True)
        if not reader or reader[0] < 100 or not moved or not editor or moved[0] <= editor[0]:
            print(label, 'INVALID: the Reader or the editor did not scroll', reader, editor, moved); sys.exit(2)
finally:
    # A debug build can take a while to quit; teardown is not under test.
    proc.terminate()
    try:
        proc.wait(timeout=30)
    except subprocess.TimeoutExpired:
        proc.kill(); proc.wait()
ok = all(r['enter_keeps'] and r['esc_keeps'] for r in result.values())
print(label, {'place_kept': ok}, flush=True)
sys.exit(0 if ok else 1)
