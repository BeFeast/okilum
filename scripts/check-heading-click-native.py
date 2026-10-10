#!/usr/bin/env python3
"""Native #978: a mouse click in a Live Preview heading reveals its `#` once the
multi-click window has passed. Run on an exclusive X11 display (never maestro).
Usage: DISPLAY=:978 python3 scripts/check-heading-click-native.py OKILUM_BINARY WORK_DIR LABEL
Readout: the left edge of the heading row (where `#` appears) is compared after
a click with the state after Left, which always reveals. Coordinates fit a
1366x768 window; set HY=0 to save a calibration screenshot instead.
"""
import os, shutil, subprocess, sys, time
from pathlib import Path

binary, work, label = sys.argv[1], Path(sys.argv[2]), sys.argv[3]
# Click in `Demo`: the H1 is twice the body size, after the note gutter (#1034).
HX, HY = int(os.environ.get('HX', '500')), int(os.environ.get('HY', '172'))
PY = int(os.environ.get('PY', '114'))  # a paragraph row
CROP = os.environ.get('CROP', '60x26+296+147')  # WxH+X+Y of the heading's left edge


def xd(env, *args):
    subprocess.run(['xdotool', *args], env=env, check=True)
    time.sleep(.3)


def shot(env, name):
    path = work / f'{label}-{name}.png'
    subprocess.run(['import', '-window', 'root', str(path)], env=env, check=True)
    if CROP:
        crop = work / f'{label}-{name}-crop.png'
        subprocess.run(['convert', str(path), '-crop', CROP, '+repage', str(crop)], check=True)
        return crop
    return path


def same(a, b):
    r = subprocess.run(['compare', '-metric', 'AE', str(a), str(b), 'null:'],
                       capture_output=True, text=True)
    return int(float(r.stderr.split()[0])) == 0


app = work / label
shutil.rmtree(app, ignore_errors=True)
(app / 'vault').mkdir(parents=True)
(app / 'vault' / 'demo.md').write_text(
    'Intro paragraph with plain words.\n\n# Okilum Demo\n\nBody paragraph after the heading.\n')
home = app / 'home'
env = dict(os.environ, WAYLAND_DISPLAY='', HOME=str(home), XDG_CONFIG_HOME=str(home / '.config'),
           XDG_DATA_HOME=str(home / 'share'), XDG_CACHE_HOME=str(home / 'cache'),
           XDG_STATE_HOME=str(home / 'state'))
proc = subprocess.Popen([binary, '--vault', str(app / 'vault'), '--index-dir', str(app / 'index'),
                         '--note', 'demo.md'], env=env, stdout=open(app / 'app.log', 'w'),
                        stderr=subprocess.STDOUT)
try:
    for _ in range(40):
        time.sleep(.25)
        found = subprocess.run(['xdotool', 'search', '--onlyvisible', '--pid', str(proc.pid)],
                               env=env, capture_output=True, text=True).stdout.split()
        if found:
            break
    win = found[-1]
    xd(env, 'windowmove', win, '0', '0', 'windowsize', win, '1366', '768', 'windowfocus', win)
    time.sleep(3)
    # Shortcuts, not toolbar coordinates: the note toolbar is contextual (#992).
    xd(env, 'key', 'ctrl+shift+e')  # Live Preview, from Reader too (#916)
    time.sleep(1.5)
    if not HY:
        shot(env, 'calibrate')
        sys.exit(0)
    xd(env, 'mousemove', str(HX), str(PY), 'click', '1')  # caret in a paragraph
    time.sleep(1.2)
    concealed = shot(env, '1-paragraph')
    xd(env, 'mousemove', str(HX), str(HY), 'click', '1')  # mid-heading
    time.sleep(1.2)  # past the 500 ms multi-click window
    clicked = shot(env, '2-click')
    xd(env, 'key', 'Left')
    time.sleep(1.2)
    revealed = shot(env, '3-left')
    result = {'left_reveals': not same(concealed, revealed),
              'click_reveals': same(clicked, revealed),
              'click_differs_from_concealed': not same(clicked, concealed)}
    print(label, result, flush=True)
    # Positive control: without it an unchanged crop proves nothing.
    assert result['left_reveals'], 'Left did not change the heading edge; crop is off'
    ok = result['click_reveals'] and result['click_differs_from_concealed']
    # Double click still selects the word under the pointer in the painted geometry.
    xd(env, 'mousemove', str(HX), str(PY), 'click', '1')
    time.sleep(1.2)
    subprocess.run(['xclip', '-i', '-selection', 'clipboard'], env=env, check=True,
                   input='<stale>', text=True)
    xd(env, 'mousemove', str(HX), str(HY), 'click', '--repeat', '2', '--delay', '80', '1')
    time.sleep(1.2)
    shot(env, '4-double')
    xd(env, 'key', 'ctrl+c')
    copied = subprocess.run(['xclip', '-o', '-selection', 'clipboard'], env=env,
                            capture_output=True, text=True).stdout
    print(label, {'double_click_copied': copied}, flush=True)
    ok &= copied == 'Demo'
    sys.exit(0 if ok else 1)
finally:
    proc.terminate()
    proc.wait(timeout=10)
