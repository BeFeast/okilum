#!/usr/bin/env python3
"""Native #1075: entering Live Preview never paints two image blocks over each
other, even in the first frames before their heights are measured.
Run on an exclusive X11 display (never maestro).
Usage: DISPLAY=:1034 python3 scripts/check-image-entry-native.py OKILUM_BINARY WORK_DIR LABEL
The vault holds two generated 300x150 SVGs (QA hit this with an SVG; solid
PNGs did not reproduce it), solid blue and solid green, with a
paragraph between them and a missing image after. The note opens in the Reader;
Ctrl+Shift+E enters Live Preview and the screen is captured as fast as possible
for about two seconds. Per frame: the rows holding blue and the rows holding
green. A paragraph separates the images, so green must start at least a text
line below blue. Overlap = green starting less than 8 rows below blue's last row
(a block painted over the one above hides its lower rows, so the bands touch).
Exit 0: no frame overlaps and the settled frame shows both images apart;
exit 1: some frame overlaps; exit 2: the run proves nothing (no window, or the
settled frame lacks an image). The pre-fix binary is the positive control: it
must exit 1, or this probe cannot see the defect.
"""
import os, shutil, subprocess, sys, time
from pathlib import Path

binary, work, label = sys.argv[1], Path(sys.argv[2]), sys.argv[3]
NOTE = ('# Images\n\nBefore the first image.\n\n![[blue.svg]]\n\nBetween the images.\n\n'
        '![[green.svg]]\n\nBefore the missing image.\n\n![[missing-picture.png]]\n\nEND\n')
CROP = (284, 96, 776, 660)
COLOURS = {'blue': '#3366cc', 'green': '#22aa44'}


def svg(path, colour, w=300, h=150):
    path.write_text(f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" '
                    f'viewBox="0 0 {w} {h}"><rect width="{w}" height="{h}" fill="{colour}"/></svg>\n')


def rows_of(path, colour):
    """Rows of the crop holding at least 40 pixels of `colour`."""
    # Everything else to white first, then the colour to black: dark text stays out.
    out = subprocess.run(['convert', str(path), '-fuzz', '6%', '-fill', 'white', '+opaque', colour,
                          '-fill', 'black', '-opaque', colour, '-scale', f'1x{CROP[3]}!',
                          '-depth', '16', 'txt:-'], capture_output=True, text=True, check=True).stdout
    width = CROP[2]
    rows = set()
    for line in out.splitlines()[1:]:
        xy, rest = line.split(':', 1)
        y = int(xy.split(',')[1])
        mean = int(rest.split('(')[1].split(')')[0].split(',')[0]) / 65535
        if (1 - mean) * width >= 40:
            rows.add(y)
    return rows


def overlaps(path):
    blue, green = rows_of(path, COLOURS['blue']), rows_of(path, COLOURS['green'])
    if not blue or not green:
        return False, blue, green
    return min(green) - max(blue) < 8, blue, green


app = work / label
shutil.rmtree(app, ignore_errors=True)
(app / 'vault').mkdir(parents=True)
(app / 'vault' / 'images.md').write_text(NOTE)
svg(app / 'vault' / 'blue.svg', COLOURS['blue'])
svg(app / 'vault' / 'green.svg', COLOURS['green'])
home = app / 'home'
env = dict(os.environ, WAYLAND_DISPLAY='', HOME=str(home), XDG_CONFIG_HOME=str(home / '.config'),
           XDG_DATA_HOME=str(home / 'share'), XDG_CACHE_HOME=str(home / 'cache'),
           XDG_STATE_HOME=str(home / 'state'))
proc = subprocess.Popen([binary, '--vault', str(app / 'vault'), '--index-dir', str(app / 'index'),
                         '--note', 'images.md'], env=env, stdout=open(app / 'app.log', 'w'),
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
    subprocess.run(['xdotool', 'windowmove', win, '0', '0', 'windowsize', win, '1366', '768',
                    'windowfocus', win], env=env, check=True)
    time.sleep(3)  # the Reader settles
    x, y, w, h = CROP
    frames = []
    subprocess.run(['xdotool', 'key', 'ctrl+shift+e'], env=env, check=True)
    start = time.monotonic()
    while time.monotonic() - start < 2:
        path = app / f'frame-{len(frames):02d}.png'
        subprocess.run(['import', '-window', 'root', '-crop', f'{w}x{h}+{x}+{y}', '+repage',
                        str(path)], env=env, check=True)
        frames.append((round(time.monotonic() - start, 3), path))
    time.sleep(2)
    settled = app / 'settled.png'
    subprocess.run(['import', '-window', 'root', '-crop', f'{w}x{h}+{x}+{y}', '+repage',
                    str(settled)], env=env, check=True)
    bad = []
    for at, path in frames:
        hit, blue, green = overlaps(path)
        if hit:
            bad.append((at, path.name, len(blue), len(green)))
    hit, blue, green = overlaps(settled)
    print(label, {'frames': len(frames), 'overlapping': bad,
                  'settled_blue_rows': len(blue), 'settled_green_rows': len(green)}, flush=True)
    if not blue or not green or hit:
        print(label, 'INVALID run: the settled frame does not show both images apart', flush=True)
        sys.exit(2)
    sys.exit(1 if bad else 0)
finally:
    proc.terminate()
    proc.wait(timeout=10)
