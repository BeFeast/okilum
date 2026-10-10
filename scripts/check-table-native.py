#!/usr/bin/env python3
"""Native #936 (S7a): a GFM table renders as a table in Live Preview, arrowing into
it reveals its Markdown source, arrowing out renders it again, the view does not
move once the table is measured, and the buffer stays byte-exact.
Run on an exclusive X11 display (never maestro).
Usage: DISPLAY=:1034 python3 scripts/check-table-native.py OKILUM_BINARY WORK_DIR LABEL
Readout: a rendered table draws horizontal borders, a run of at least 200 px of
non-page pixels on one row; raw text never does (its `|---|` is a few short
dashes). Source mode is the control: the same note there must show no such
border. Exit 1: a step failed; exit 2: the run proves nothing.
"""
import os, shutil, subprocess, sys, time
from pathlib import Path

binary, work, label = sys.argv[1], Path(sys.argv[2]), sys.argv[3]
NOTE = ('Intro line above the table.\n\n'
        '| Name | Count | Note |\n|---|---|---|\n| apple | 3 | crisp |\n| שלום | 5 | עברית |\n'
        '| pear | 12 | a much longer cell that still fits |\n\n'
        'Text after the table.\n')
CROP = (284, 96, 776, 360)


def xd(env, *args):
    subprocess.run(['xdotool', *args], env=env, check=True)
    time.sleep(.3)


def shot(env, name):
    path = work / f'{label}-{name}.png'
    subprocess.run(['import', '-window', 'root', str(path)], env=env, check=True)
    return path


def border_rows(path):
    """Rows of the crop holding a run of >= 200 px that differ from the page."""
    x0, y0, w, h = CROP
    out = subprocess.run(['convert', str(path), '-crop', f'{w}x{h}+{x0}+{y0}', '+repage',
                          '-colorspace', 'Gray', '-depth', '8', 'txt:-'],
                         capture_output=True, text=True, check=True).stdout
    rows = {}
    for line in out.splitlines()[1:]:
        xy, rest = line.split(':', 1)
        px, py = map(int, xy.split(','))
        rows.setdefault(py, {})[px] = int(rest.split('(')[1].split(')')[0].split(',')[0])
    page = max(set(v for row in rows.values() for v in row.values()),
               key=lambda v: sum(1 for row in rows.values() for x in row.values() if x == v))
    found = []
    for py, row in rows.items():
        run = best = 0
        for px in range(w):
            if abs(row.get(px, page) - page) > 6:
                run += 1
                best = max(best, run)
            else:
                run = 0
        if best >= 200:
            found.append(y0 + py)
    return sorted(found)


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
    (app / 'vault' / 'table.md').write_text(NOTE)
    home = app / 'home'
    env = dict(os.environ, WAYLAND_DISPLAY='', HOME=str(home), XDG_CONFIG_HOME=str(home / '.config'),
               XDG_DATA_HOME=str(home / 'share'), XDG_CACHE_HOME=str(home / 'cache'),
               XDG_STATE_HOME=str(home / 'state'))
    proc = subprocess.Popen([binary, '--vault', str(app / 'vault'), '--index-dir', str(app / 'index'),
                             '--note', 'table.md'], env=env, stdout=open(app / 'app.log', 'w'),
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
        xd(env, 'key', 'ctrl+Home')  # caret on the intro line, outside the table
        time.sleep(1)
        opened = shot(env, f'{mode}-1-open')
        time.sleep(1)
        settled = shot(env, f'{mode}-2-settled')
        # The caret blinks; anything wider than a caret means the view moved.
        changed = subprocess.run(['convert', str(opened), str(settled), '-compose', 'difference',
                                  '-composite', '-threshold', '5%', '-trim', '-format', '%w %h %@',
                                  'info:'], capture_output=True, text=True).stdout.split()
        width = int(changed[0]) if changed else 0
        result = {'rendered': bool(border_rows(settled)), 'still': width <= 3,
                  'changed': ' '.join(changed)}
        if mode == 'live':
            for _ in range(3):  # intro, blank, then the table's first line
                xd(env, 'key', 'Down')
            time.sleep(1)
            result['revealed'] = not border_rows(shot(env, f'{mode}-3-inside'))
            for _ in range(7):  # out below the table
                xd(env, 'key', 'Down')
            time.sleep(1)
            result['rendered_again'] = bool(border_rows(shot(env, f'{mode}-4-after')))
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
# Control: Source shows the raw table, so no border run.
if source['rendered']:
    print(label, 'INVALID run: border detector fires on raw text', flush=True)
    sys.exit(2)
ok = live['rendered'] and live['still'] and live['revealed'] and live['rendered_again'] and live['exact']
sys.exit(0 if ok else 1)
