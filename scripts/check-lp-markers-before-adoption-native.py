#!/usr/bin/env python3
"""Native #1295/#1266: Live Preview draws the same frame before and after the
background parse of a keystroke. Run on an exclusive X11 display (never maestro).
Usage: DISPLAY=:1037 python3 scripts/check-lp-markers-before-adoption-native.py NATIVE_MARKER784 OUT LABEL [CASE]
CASE (default mid-list):
- mid-list (#1295): the end of the wrapped second-level item, a deeper item below;
- last-nested (#1266): the end of the last, nested item, a blank line and a quote below;
- task (#1266): the end of a task list right under an ordered one;
- note-end (#1323): the stable gate's note, a heading, a quote and a list
  ending the note without a final newline; at the end of the last item type,
  and 0.2 s later (drawn, not yet parsed) press Enter, type again, then Undo
  twice. The fixture loads the note from OKILUM_NATIVE216_SOURCE.
The isolated #216 fixture with its 500 ms parse delay holds the unadopted
revision on screen long enough to capture it even on Xvfb, where the real
one-frame gap is never painted. With the caret at the case's place: Enter
(it keeps the line's or the deeper next line's indent), `**x**`, Backspace
twice. Before adoption each frame must look exactly like the adopted one
above and below the edited line (the edited line's own syntax waits for
the parse).
Positive control: the capture inside the window is taken while the status
line still says the source is being classified (the edit is unadopted), and
the typed text is on screen.
Exit 0: all hold; 1: a check failed; 2: the run proves nothing.
"""
import os, subprocess, sys, time
from pathlib import Path

binary, out, label = sys.argv[1], Path(sys.argv[2]), sys.argv[3]
case = sys.argv[4] if len(sys.argv) > 4 else 'mid-list'
out.mkdir(parents=True, exist_ok=True)
env = dict(os.environ, WAYLAND_DISPLAY='')
# 900x900 window; text rows are 24 px. Per case: caret click, the rows above
# the edit, and the rows below the new edited line.
CASES = {
    'mid-list': ((400, 386), '840x122+20+275', '840x130+20+422'),
    'last-nested': ((400, 410), '840x146+20+275', '840x130+20+446'),
    'task': ((420, 626), '840x362+20+275', '840x120+20+662'),
}
CASES['note-end'] = ((400, 433), '840x150+20+280', '840x60+20+500')
NOTE_END = '# Quote and list\n\n> A quote before the list.\n\n- first item\n- last item'
STEPS = {'note-end': [('type-enter', 'ab'), ('type', 'n'), ('key', 'ctrl+z'), ('key', 'ctrl+z')]}
CARET, ABOVE, BELOW = CASES[case]
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


if case == 'note-end':
    (out / 'note-end.md').write_text(NOTE_END)
    env['OKILUM_NATIVE216_SOURCE'] = str(out / 'note-end.md')
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
    xd('mousemove', *CARET, 'click', 1)
    time.sleep(2)
    steps = STEPS.get(case) or [('key', 'Return')] + [('type', c) for c in '**x**'] + [('key', 'BackSpace')] * 2
    for i, (kind, arg) in enumerate(steps):
        if kind == 'type-enter':
            xd('type', arg)
            time.sleep(.2)
            xd('key', 'Return')
        else:
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
        results[i] = {'step': f'{kind} {arg!r}', 'unadopted_capture': unadopted,
                      'no_flash': same(during[0], after[0]) and same(during[1], after[1])}
        print(label, results[i], flush=True)
finally:
    proc.terminate(); proc.wait(timeout=10)

if not all(r['unadopted_capture'] for r in results.values()):
    print(label, 'INVALID: a capture was not taken before adoption'); sys.exit(2)
ok = all(r['no_flash'] for r in results.values())
print(label, case, {'same_frame_before_adoption': ok}, flush=True)
sys.exit(0 if ok else 1)
