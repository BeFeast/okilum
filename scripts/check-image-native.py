#!/usr/bin/env python3
"""Native #936 (S7b): an image embed renders as the image in Live Preview, arrowing
into its line reveals the Markdown, arrowing out draws it again, and the buffer
stays byte-exact. Run on an exclusive X11 display (never maestro).
Usage: DISPLAY=:1034 python3 scripts/check-image-native.py OKILUM_BINARY WORK_DIR LABEL
Readout: the vault holds a generated 300x150 solid #3366cc PNG; the check counts
pixels of that colour in the note pane. Source is the control: it shows the raw
`![[pic.png]]`, so no image pixels. Exit 1: a step failed; exit 2: the run proves
nothing.
"""
import os, shutil, subprocess, sys, time
from pathlib import Path

binary, work, label = sys.argv[1], Path(sys.argv[2]), sys.argv[3]
NOTE = 'Intro line above the image.\n\n![[pic.png]]\n\nText after the image.\n'
CROP = (284, 96, 776, 500)
BLUE = (0x33, 0x66, 0xcc)


def xd(env, *args):
    subprocess.run(['xdotool', *args], env=env, check=True)
    time.sleep(.3)


def blue_pixels(env, name):
    path = work / f'{label}-{name}.png'
    subprocess.run(['import', '-window', 'root', str(path)], env=env, check=True)
    x, y, w, h = CROP
    out = subprocess.run(['convert', str(path), '-crop', f'{w}x{h}+{x}+{y}', '+repage',
                          '-fuzz', '6%', '-fill', 'white', '+opaque', '#3366cc',
                          '-fill', 'black', '-opaque', '#3366cc', '-format', '%[fx:mean*w*h]',
                          'info:'], capture_output=True, text=True, check=True).stdout
    # Pixels of the image colour turned black; mean counts white, so invert.
    return w * h - round(float(out))


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
    (app / 'vault' / 'image.md').write_text(NOTE)
    subprocess.run(['convert', '-size', '300x150', 'xc:#3366cc', str(app / 'vault' / 'pic.png')],
                   check=True)
    home = app / 'home'
    env = dict(os.environ, WAYLAND_DISPLAY='', HOME=str(home), XDG_CONFIG_HOME=str(home / '.config'),
               XDG_DATA_HOME=str(home / 'share'), XDG_CACHE_HOME=str(home / 'cache'),
               XDG_STATE_HOME=str(home / 'state'))
    proc = subprocess.Popen([binary, '--vault', str(app / 'vault'), '--index-dir', str(app / 'index'),
                             '--note', 'image.md'], env=env, stdout=open(app / 'app.log', 'w'),
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
        time.sleep(1.5)  # the image loads asynchronously
        result = {'image_pixels': blue_pixels(env, f'{mode}-1-open')}
        if mode == 'live':
            for _ in range(2):  # intro, blank, then the embed line
                xd(env, 'key', 'Down')
            time.sleep(1)
            result['inside_pixels'] = blue_pixels(env, f'{mode}-2-inside')
            for _ in range(2):  # out below the image
                xd(env, 'key', 'Down')
            time.sleep(1)
            result['after_pixels'] = blue_pixels(env, f'{mode}-3-after')
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
# The full image is 45000 px; allow scaling to the column and antialiasing.
drawn = lambda n: n > 20000
if drawn(source['image_pixels']):
    print(label, 'INVALID run: image colour present in Source', flush=True)
    sys.exit(2)
ok = (drawn(live['image_pixels']) and live['inside_pixels'] < 1000
      and drawn(live['after_pixels']) and live['exact'])
sys.exit(0 if ok else 1)
