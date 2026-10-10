#!/usr/bin/env python3
"""Native #936 (S7c): an Obsidian callout renders as a callout in Live Preview,
arrowing into it reveals its Markdown, arrowing out renders it again, and the
buffer stays byte-exact. Run on an exclusive X11 display (never maestro).
Usage: DISPLAY=:1034 python3 scripts/check-callout-native.py OKILUM_BINARY WORK_DIR LABEL
Readout: a rendered callout has a coloured border and tint; raw quote text and
its bar are grey. The check counts saturated pixels in the note pane. Source is
the baseline: the raw callout adds no colour there. Exit 1: a step failed; exit 2: the
run proves nothing.
"""
import os, shutil, subprocess, sys, time
from pathlib import Path

binary, work, label = sys.argv[1], Path(sys.argv[2]), sys.argv[3]
NOTE = ('Intro line above the callout.\n\n'
        '> [!warning] Careful\n> The body of the callout, long enough to read.\n> Second line.\n\n'
        'Text after the callout.\n')
CROP = (284, 96, 776, 360)


def xd(env, *args):
    subprocess.run(['xdotool', *args], env=env, check=True)
    time.sleep(.3)


def coloured(env, name):
    path = work / f'{label}-{name}.png'
    subprocess.run(['import', '-window', 'root', str(path)], env=env, check=True)
    x, y, w, h = CROP
    out = subprocess.run(['convert', str(path), '-crop', f'{w}x{h}+{x}+{y}', '+repage',
                          '-colorspace', 'HSL', '-channel', 'G', '-separate', '+channel',
                          '-threshold', '25%', '-format', '%[fx:mean*w*h]', 'info:'],
                         capture_output=True, text=True, check=True).stdout
    return round(float(out))


def copy_all(env):
    subprocess.run(['xclip', '-i', '-selection', 'clipboard'], env=env, check=True,
                   input='<stale>', text=True)
    xd(env, 'key', 'ctrl+a')
    xd(env, 'key', 'ctrl+c')
    time.sleep(.5)
    copied = subprocess.run(['xclip', '-o', '-selection', 'clipboard'], env=env,
                            capture_output=True, text=True).stdout
    xd(env, 'key', 'ctrl+Home')
    return copied


def run(mode):
    app = work / label / mode
    shutil.rmtree(app, ignore_errors=True)
    (app / 'vault').mkdir(parents=True)
    (app / 'vault' / 'callout.md').write_text(NOTE)
    home = app / 'home'
    env = dict(os.environ, WAYLAND_DISPLAY='', HOME=str(home), XDG_CONFIG_HOME=str(home / '.config'),
               XDG_DATA_HOME=str(home / 'share'), XDG_CACHE_HOME=str(home / 'cache'),
               XDG_STATE_HOME=str(home / 'state'))
    proc = subprocess.Popen([binary, '--vault', str(app / 'vault'), '--index-dir', str(app / 'index'),
                             '--note', 'callout.md'], env=env, stdout=open(app / 'app.log', 'w'),
                            stderr=subprocess.STDOUT)
    try:
        for _ in range(40):
            time.sleep(.25)
            found = subprocess.run(['xdotool', 'search', '--onlyvisible', '--pid', str(proc.pid)],
                                   env=env, capture_output=True, text=True).stdout.split()
            if found:
                break
        else:
            return None
        win = found[-1]
        xd(env, 'windowmove', win, '0', '0', 'windowsize', win, '1366', '768', 'windowfocus', win)
        time.sleep(3)
        xd(env, 'key', 'ctrl+shift+e' if mode == 'live' else 'ctrl+e')
        time.sleep(1.5)
        xd(env, 'key', 'ctrl+Home')
        time.sleep(1)
        result = {'open': coloured(env, f'{mode}-1-open')}
        if mode == 'live':
            for _ in range(2):  # intro, blank, then the callout's first line
                xd(env, 'key', 'Down')
            time.sleep(1)
            result['inside'] = coloured(env, f'{mode}-2-inside')
            for _ in range(4):  # out below the callout
                xd(env, 'key', 'Down')
            time.sleep(1)
            result['after'] = coloured(env, f'{mode}-3-after')
        result['exact'] = copy_all(env) == NOTE
        return result
    finally:
        proc.terminate()
        proc.wait(timeout=10)


source, live = run('source'), run('live')
print(label, 'source', source, flush=True)
print(label, 'live', live, flush=True)
if not source or not live or not source['exact']:
    sys.exit(2)
# Chrome around the note can be coloured too (vault tint): Source is the
# baseline, and the rendered callout adds well over 3000 coloured pixels.
base = source['open']
rendered = lambda n: n > base + 3000
ok = (rendered(live['open']) and live['inside'] < base + 1000
      and rendered(live['after']) and live['exact'])
sys.exit(0 if ok else 1)
