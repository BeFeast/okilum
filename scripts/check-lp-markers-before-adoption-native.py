#!/usr/bin/env python3
"""Native #1295: list and quote markers stay drawn between a keystroke and the
background parse. Run on an exclusive X11 display (never maestro).
Usage: DISPLAY=:1037 python3 scripts/check-lp-markers-before-adoption-native.py NATIVE_MARKER784 OUT LABEL
The isolated #216 fixture with its 500 ms parse delay holds the unadopted
revision on screen long enough to capture it even on Xvfb, where the real
one-frame gap is never painted. As on Hedva, the caret is at the end of the
second-level list item, which has a deeper item below it: Enter (it keeps the
deeper indent), `**x**`, Backspace twice. Before adoption each frame must
look exactly like the adopted one: the list rows above the edit, and the
deeper item and the quote below it.
Positive control: the capture inside the window is taken while the status
line still says the source is being classified (the edit is unadopted), and
the typed text is on screen.
Exit 0: all hold; 1: a check failed; 2: the run proves nothing.
"""
import os, subprocess, sys, time
from pathlib import Path

binary, out, label = sys.argv[1], Path(sys.argv[2]), sys.argv[3]
out.mkdir(parents=True, exist_ok=True)
env = dict(os.environ, WAYLAND_DISPLAY='')
ABOVE = '840x122+20+275'     # title, first level and the wrapped second level
BELOW = '840x130+20+422'     # third level, the quote and the rule, below the edited line
STATUS = '880x24+10+176'     # "Classifying in background" vs "Current classification"


def xd(*args):
    subprocess.run(['xdotool', *map(str, args)], env=env, check=True)


def grab(name, crop):
    path = out / f'{label}-{name}.png'
    subprocess.run(['import', '-window', 'root', '-crop', crop, str(path)], env=env, check=True)
    return path


def same(a, b):
    r = subprocess.run(['compare', '-metric', 'AE', str(a), str(b), 'null:'], env=env,
                       capture_output=True, text=True)
    return int(float(r.stderr.split()[0] or 1)) == 0


proc = subprocess.Popen([binary], env=env, stdout=open(out / f'{label}.log', 'w'), stderr=subprocess.STDOUT)
results = {}
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
    xd('windowmove', win, 0, 0, 'windowsize', win, 900, 900)
    time.sleep(4)
    xd('mousemove', 729, 73, 'click', 1)          # Parse delay: 500 ms
    time.sleep(1)
    xd('mousemove', 400, 386, 'click', 1)         # end of the wrapped second level
    time.sleep(2)
    steps = [('key', 'Return')] + [('type', c) for c in '**x**'] + [('key', 'BackSpace')] * 2
    for i, (kind, arg) in enumerate(steps):
        xd(kind, arg)
        time.sleep(.12)
        during = (grab(f'{i}-during-above', ABOVE), grab(f'{i}-during-below', BELOW))
        status = grab(f'{i}-status', STATUS)
        time.sleep(1.2)
        after = (grab(f'{i}-after-above', ABOVE), grab(f'{i}-after-below', BELOW))
        settled = grab(f'{i}-status-after', STATUS)
        # Positive control: the status line changed between the two captures,
        # so the first was taken before adoption.
        unadopted = not same(status, settled)
        results[i] = {'step': f'{kind} {arg}', 'unadopted_capture': unadopted,
                      'no_flash': same(during[0], after[0]) and same(during[1], after[1])}
        print(label, results[i], flush=True)
finally:
    proc.terminate(); proc.wait(timeout=10)

if not all(r['unadopted_capture'] for r in results.values()):
    print(label, 'INVALID: a capture was not taken before adoption'); sys.exit(2)
ok = all(r['no_flash'] for r in results.values())
print(label, {'markers_kept_before_adoption': ok}, flush=True)
sys.exit(0 if ok else 1)
