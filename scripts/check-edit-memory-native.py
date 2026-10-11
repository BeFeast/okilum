#!/usr/bin/env python3
"""Native #1237: memory does not grow across repeated rapid edits in Live
Preview. Run on an exclusive X11 display (never maestro).
Usage: DISPLAY=:1034 python3 scripts/check-edit-memory-native.py OKILUM_BINARY WORK_DIR LABEL [CYCLES] [live|source]
Run it in Source as well: the same cycles without Live Preview's projection
separate growth that belongs to Live Preview from the editor's own.
The note is the L1 long nested corpus (180 list-with-quote units, ~16 KB, like
the QA fixture l1-long-180.md). Narrow (700 px), dark. One cycle, as in the QA
step: type 20 characters, Enter x5, then Undo until the saved note is the
original again. RSS is read after each cycle once the app has settled.
Readout: the RSS slope over the restored cycles after warm-up (first 5 dropped).
Pass: under 64 KiB per cycle, and the saved note is the original at the end.
Positive control: the RSS reading is live (it rises when Live Preview first
renders the long note).
Exit 0: all hold; 1: a check failed; 2: the run proves nothing.
"""
import json, os, shutil, subprocess, sys, time
from pathlib import Path

binary, work, label = sys.argv[1], Path(sys.argv[2]), sys.argv[3]
cycles = int(sys.argv[4]) if len(sys.argv) > 4 else 30
mode = sys.argv[5] if len(sys.argv) > 5 else 'live'
UNIT = '- item **bold** [label](https://example.com/x)\n  > quote *q* [[Target|alias]]\n\npara **p**\n\n'
NOTE = '# Long nested note\n\n' + UNIT * 180 + 'END-OF-LONG-180\n'


def xd(env, *args):
    subprocess.run(['xdotool', *map(str, args)], env=env, check=True)


def rss(pid):
    return int(Path(f'/proc/{pid}/status').read_text().split('VmRSS:')[1].split()[0])  # KiB


root = work / label
shutil.rmtree(root, ignore_errors=True)
(root / 'vault').mkdir(parents=True)
note = root / 'vault' / 'long.md'
note.write_text(NOTE)
home = root / 'home'
(home / '.config' / 'okilum').mkdir(parents=True)
(home / '.config' / 'okilum' / 'appearance.json').write_text(json.dumps({'appearance': 'dark'}))
env = dict(os.environ, WAYLAND_DISPLAY='', HOME=str(home), XDG_CONFIG_HOME=str(home / '.config'),
           XDG_DATA_HOME=str(home / 'share'), XDG_CACHE_HOME=str(home / 'cache'), XDG_STATE_HOME=str(home / 'state'))
proc = subprocess.Popen([binary, '--vault', str(root / 'vault'), '--index-dir', str(root / 'index'),
                         '--note', 'long.md'], env=env, stdout=open(root / 'app.log', 'w'), stderr=subprocess.STDOUT)
readings, restored = [], []
try:
    for _ in range(40):
        time.sleep(.25)
        found = subprocess.run(['xdotool', 'search', '--onlyvisible', '--pid', str(proc.pid)], env=env,
                               capture_output=True, text=True).stdout.split()
        if found:
            break
    else:
        print(label, 'INVALID: no window', flush=True)
        sys.exit(2)
    win = found[-1]
    xd(env, 'windowmove', win, 0, 0, 'windowsize', win, 700, 700, 'windowfocus', win)
    time.sleep(6)
    empty_rss = rss(proc.pid)  # the Reader on the note, before the editor exists
    xd(env, 'key', 'ctrl+shift+e' if mode == 'live' else 'ctrl+e')
    time.sleep(4)
    open_rss = rss(proc.pid)
    for cycle in range(cycles):
        xd(env, 'key', 'ctrl+Home')
        xd(env, 'key', '--delay', '20', *(['Down'] * 6))
        xd(env, 'key', 'End')
        xd(env, 'type', '--delay', '15', 'abcdefghijklmnopqrst')
        xd(env, 'key', '--delay', '30', *(['Return'] * 5))
        for _ in range(60):
            xd(env, 'key', 'ctrl+s')
            time.sleep(.6)
            if note.read_text() == NOTE:
                break
            xd(env, 'key', 'ctrl+z')
            time.sleep(.2)
        restored.append(note.read_text() == NOTE)
        time.sleep(3)
        readings.append(rss(proc.pid))
        print(label, 'cycle', cycle, 'rss_kib', readings[-1], 'restored', restored[-1], flush=True)
finally:
    proc.terminate()
    proc.wait(timeout=10)

# Growth is judged on restored cycles only: a cycle that left text behind
# makes the note (and so the RSS) larger by design.
tail = [r for r, ok in list(zip(readings, restored))[5:] if ok]
n = len(tail)
mean_x, mean_y = (n - 1) / 2, sum(tail) / n
slope = sum((i - mean_x) * (y - mean_y) for i, y in enumerate(tail)) / sum((i - mean_x) ** 2 for i in range(n))
result = {'empty_rss': empty_rss, 'open_rss': open_rss, 'first': readings[0], 'last': readings[-1],
          'slope_kib_per_cycle': round(slope, 1), 'all_restored': all(restored)}
print(label, result, flush=True)
if open_rss <= empty_rss:
    print(label, 'INVALID: RSS did not rise when the editor opened', flush=True)
    sys.exit(2)
checks = {'no_growth': slope < 64, 'note_restored_every_cycle': all(restored)}
print(label, checks, flush=True)
sys.exit(0 if all(checks.values()) else 1)
