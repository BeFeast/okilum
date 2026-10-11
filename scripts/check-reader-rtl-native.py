#!/usr/bin/env python3
"""Native #1053: in the Reader a Hebrew paragraph is right-aligned at the same
right edge as in Live Preview; a double-click on it selects a Hebrew word, and
a link inside a Hebrew paragraph opens on click.
Run on an exclusive X11 display (never maestro).
Usage: DISPLAY=:1034 python3 scripts/check-reader-rtl-native.py OKILUM_BINARY WORK_DIR LABEL
Readout: ink bands of the note column. The long Latin paragraph gives the text
start and the wrap edge (control). The Hebrew paragraph must end at the wrap
edge, within a few pixels of where Live Preview draws it, and start well right
of the text start. Then, in the Reader: double-click the middle of the Hebrew
row and copy (one Hebrew word expected), and click the link's coloured text in
the second Hebrew paragraph (the target note expected).
Exit 0: all hold; 1: a check failed; 2: the run proves nothing.
"""
import os, shutil, subprocess, sys, time
from pathlib import Path

binary, work, label = sys.argv[1], Path(sys.argv[2]), sys.argv[3]
LATIN = ('A long Latin paragraph that wraps across the column width so that its first row '
         'shows where the wrap edge is, again and again and again and again and again.')
HEBREW = 'שלום עולם זה טקסט'
LINKED = 'קישור אל [[target|היעד]] כאן'
NOTE = f'{LATIN}\n\n{HEBREW}\n\n{LINKED}\n\nshort latin\n'
CROP = (284, 96, 776, 400)


def xd(env, *args):
    subprocess.run(['xdotool', *args], env=env, check=True)
    time.sleep(.3)


def pixels(path):
    x0, y0, w, h = CROP
    out = subprocess.run(['convert', str(path), '-crop', f'{w}x{h}+{x0}+{y0}', '+repage',
                          '-depth', '8', 'txt:-'], capture_output=True, text=True, check=True).stdout
    for line in out.splitlines()[1:]:
        xy, rest = line.split(':', 1)
        px, py = map(int, xy.split(','))
        r, g, b = (int(v) for v in rest.split('(')[1].split(')')[0].split(',')[:3])
        yield x0 + px, y0 + py, r, g, b


def bands(path):
    rows = {}
    for x, y, r, g, b in pixels(path):
        if (r + g + b) / 3 < 140 and x > CROP[0] + 3:
            lo, hi = rows.get(y, (10**6, -1))
            rows[y] = (min(lo, x), max(hi, x))
    out, cur = [], None
    for y in sorted(rows):
        if cur and y - cur[1] <= 2:
            cur = [cur[0], y, min(cur[2], rows[y][0]), max(cur[3], rows[y][1])]
        else:
            if cur and cur[1] - cur[0] >= 5:
                out.append(cur)
            cur = [y, y, rows[y][0], rows[y][1]]
    if cur and cur[1] - cur[0] >= 5:
        out.append(cur)
    return out  # [top, bottom, left, right]


def link_centre(path, band):
    """Centre of the link-coloured (blue) pixels inside a band."""
    blue = [(x, y) for x, y, r, g, b in pixels(path)
            if band[0] - 2 <= y <= band[1] + 2 and b > 140 and b > r + 50]
    if not blue:
        return None
    xs, ys = sorted(p[0] for p in blue), sorted(p[1] for p in blue)
    return xs[len(xs) // 2], ys[len(ys) // 2]


def copy(env, select_all):
    subprocess.run(['xclip', '-i', '-selection', 'clipboard'], env=env, check=True,
                   input='<stale>', text=True)
    if select_all:
        xd(env, 'key', 'ctrl+a')
    xd(env, 'key', 'ctrl+c')
    time.sleep(.5)
    return subprocess.run(['xclip', '-o', '-selection', 'clipboard'], env=env,
                          capture_output=True, text=True).stdout


def run(mode):
    app = work / label / mode
    shutil.rmtree(app, ignore_errors=True)
    (app / 'vault').mkdir(parents=True)
    (app / 'vault' / 'rtl.md').write_text(NOTE)
    (app / 'vault' / 'target.md').write_text('# Target page\n\nArrived.\n')
    home = app / 'home'
    env = dict(os.environ, WAYLAND_DISPLAY='', HOME=str(home), XDG_CONFIG_HOME=str(home / '.config'),
               XDG_DATA_HOME=str(home / 'share'), XDG_CACHE_HOME=str(home / 'cache'),
               XDG_STATE_HOME=str(home / 'state'))
    proc = subprocess.Popen([binary, '--vault', str(app / 'vault'), '--index-dir', str(app / 'index'),
                             '--note', 'rtl.md'], env=env, stdout=open(app / 'app.log', 'w'),
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
        time.sleep(6)
        if mode == 'live':
            xd(env, 'key', 'ctrl+shift+e')
            time.sleep(3)
            xd(env, 'key', 'ctrl+End')  # caret on the last line, away from both Hebrew lines
            time.sleep(1.5)
        shot = work / f'{label}-{mode}.png'
        subprocess.run(['import', '-window', 'root', str(shot)], env=env, check=True)
        found_bands = bands(shot)
        if len(found_bands) < 4:
            return {'bands': found_bands}
        latin_edge = max(band[3] for band in found_bands[:-3])
        result = {'text_left': found_bands[0][2], 'latin_edge': latin_edge,
                  'hebrew': found_bands[-3], 'linked': found_bands[-2]}
        if mode == 'reader':
            hebrew, latin = found_bands[-3], found_bands[0]

            def double_click(x, y):
                xd(env, 'mousemove', str(x), str(y), 'click', '--repeat', '2', '--delay', '80', '1')
                time.sleep(.8)
                word = copy(env, False).strip()
                xd(env, 'key', 'Escape')
                return word

            # Positive control: a Latin word selects by double-click.
            result['latin_word'] = double_click(latin[2] + 30, (latin[0] + latin[1]) // 2)
            # The Hebrew row's middle word (the row is right-aligned).
            result['word'] = double_click((hebrew[2] + hebrew[3]) // 2, (hebrew[0] + hebrew[1]) // 2)
            # The link last: it navigates away.
            link = link_centre(shot, found_bands[-2])
            result['link_at'] = link
            if link:
                xd(env, 'mousemove', str(link[0]), str(link[1]), 'click', '1')
                time.sleep(3)
                result['after_link'] = copy(env, True)[:60]
        return result
    finally:
        proc.terminate()
        proc.wait(timeout=10)


reader, live = run('reader'), run('live')
print(label, 'reader', reader, flush=True)
print(label, 'live', live, flush=True)
if not reader or not live or 'hebrew' not in reader or 'hebrew' not in live:
    sys.exit(2)
if not reader.get('link_at'):
    print(label, 'INVALID run: no link pixels found', flush=True)
    sys.exit(2)
if reader.get('latin_word') not in LATIN.replace(',', ' ').replace('.', ' ').split():
    print(label, 'INVALID run: double-click selects no Latin word either', flush=True)
    sys.exit(2)
heb = reader['hebrew']
right_aligned = (reader['latin_edge'] - 2 <= heb[3] <= reader['latin_edge'] + 80
                 and heb[2] > reader['text_left'] + 150)
same_edge = abs(heb[3] - live['hebrew'][3]) <= 12
word_ok = reader.get('word') in HEBREW.split()
link_ok = 'Target page' in reader.get('after_link', '') or 'Arrived' in reader.get('after_link', '')
checks = {'right_aligned': right_aligned, 'same_edge_as_live': same_edge,
          'double_click_word': word_ok, 'link_opens': link_ok}
print(label, checks, flush=True)
sys.exit(0 if all(checks.values()) else 1)
