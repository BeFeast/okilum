#!/usr/bin/env python3
"""Native #979: after Dark -> Light with the caret placed in a Live Preview heading
by a mouse click, the heading uses the Light palette at once. Run on an exclusive
X11 display (never maestro).
Usage: DISPLAY=:979 python3 scripts/check-theme-heading-native.py OKILUM_BINARY WORK_DIR LABEL
Readout: the darkest ink in the heading row compared with the body paragraph,
which switches palette correctly (positive control). The heading crop stops
left of the caret. Coordinates fit a 1366x768 window; set LX=0 to save a
calibration screenshot of the Settings window instead.
"""
import json, os, shutil, subprocess, sys, time
from pathlib import Path

binary, work, label = sys.argv[1], Path(sys.argv[2]), sys.argv[3]
HX, HY, PY = 385, 160, 114
LX, LY = int(os.environ.get('LX', '948')), int(os.environ.get('LY', '217'))  # settings Light
HEADING, BODY = '64x24+296+148', '260x24+296+102'


def xd(env, *args):
    subprocess.run(['xdotool', *args], env=env, check=True)
    time.sleep(.3)


def shot(env, name):
    path = work / f'{label}-{name}.png'
    subprocess.run(['import', '-window', 'root', str(path)], env=env, check=True)
    return path


def gray(path, crop, fx):
    """minima or mean intensity of the crop, 0..1."""
    out = subprocess.run(['convert', str(path), '-crop', crop, '+repage', '-colorspace', 'Gray',
                          '-format', f'%[fx:{fx}]', 'info:'],
                         capture_output=True, text=True, check=True).stdout
    return round(float(out), 3)


app = work / label
shutil.rmtree(app, ignore_errors=True)
(app / 'vault').mkdir(parents=True)
(app / 'vault' / 'demo.md').write_text(
    'Intro paragraph with plain words.\n\n# Okilum Demo\n\nBody paragraph after the heading.\n')
home = app / 'home'
(home / '.config' / 'okilum').mkdir(parents=True)
(home / '.config' / 'okilum' / 'appearance.json').write_text(json.dumps({'appearance': 'dark'}))
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
    xd(env, 'mousemove', '996', '70', 'click', '1')  # Edit
    time.sleep(1.5)
    xd(env, 'mousemove', '968', '70', 'click', '1')  # Live Preview
    time.sleep(1.5)
    xd(env, 'mousemove', str(HX), str(HY), 'click', '1')  # caret mid-heading, by mouse
    time.sleep(1.2)
    dark = shot(env, '1-dark')
    xd(env, 'key', 'ctrl+comma')  # opens a separate Settings window
    time.sleep(2)
    if not LX:
        shot(env, 'settings')
        sys.exit(0)
    xd(env, 'mousemove', str(LX), str(LY), 'click', '1')  # Light
    time.sleep(1)
    # Settings is its own window; without a window manager focus must be explicit.
    windows = subprocess.run(['xdotool', 'search', '--onlyvisible', '--pid', str(proc.pid)],
                             env=env, capture_output=True, text=True).stdout.split()
    settings = [w for w in windows if w != win]
    assert settings, 'no settings window'
    xd(env, 'windowfocus', '--sync', settings[-1], 'key', 'ctrl+w')
    time.sleep(1)
    assert len(subprocess.run(['xdotool', 'search', '--onlyvisible', '--pid', str(proc.pid)],
                              env=env, capture_output=True, text=True).stdout.split()) == 1
    xd(env, 'windowfocus', win)
    time.sleep(1)
    light = shot(env, '2-light')
    result = {'dark_mean': gray(dark, BODY, 'mean'), 'light_mean': gray(light, BODY, 'mean'),
              'light_body_ink': gray(light, BODY, 'minima'),
              'light_heading_ink': gray(light, HEADING, 'minima')}
    print(label, result, flush=True)
    # Positive controls: it started Dark, ended Light, and body ink is dark.
    assert result['dark_mean'] < .4 < .7 < result['light_mean'], 'theme did not switch'
    assert result['light_body_ink'] < .3, 'body ink unreadable; crop is off'
    ok = result['light_heading_ink'] < result['light_body_ink'] + .15
    sys.exit(0 if ok else 1)
finally:
    proc.terminate()
    proc.wait(timeout=10)
