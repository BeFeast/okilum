#!/usr/bin/env python3
"""Native #1237: an external link opens from Live Preview and from the Reader.
Runs in the headless Wayland rig (scripts/wayland_rig.py); never on maestro.
Usage: python3 scripts/check-external-link-native.py OKILUM_BINARY WORK_DIR LABEL
A stub `xdg-open` first on PATH records each URL Okilum hands to the system
(gpui opens URLs through xdg-open); no browser is needed.
Scenarios, each in a fresh launch:
- Live Preview, Super+click on the link text: the URL is handed over exactly
  once and the note's bytes do not change;
- Live Preview, plain click on the link text: nothing is handed over (control:
  only the platform modifier follows links in the editor);
- Reader, plain click on the link text: the URL is handed over.
Exit 0: all hold; 1: a check failed; 2: the run proves nothing.
"""
import os, shutil, subprocess, sys, time
from pathlib import Path

from wayland_rig import Rig

binary, work, label = sys.argv[1], Path(sys.argv[2]), sys.argv[3]
URL = 'https://example.com/okilum-1237?q=a&b=c'
NOTE = f'# Links\n\nPlain words, then [the external site]({URL}) and more words.\n\nlast line\n'


def link_centre(path):
    """Centre of the link-coloured pixels in the note column (x 290..1080)."""
    out = subprocess.run(['convert', str(path), '-crop', '790x400+290+92', '+repage', '-depth', '8', 'txt:-'],
                         capture_output=True, text=True, check=True).stdout
    pts = []
    for line in out.splitlines()[1:]:
        xy, rest = line.split(':', 1)
        r, g, b = (int(v) for v in rest.split('(')[1].split(')')[0].split(',')[:3])
        if b > 140 and b > r + 50:
            x, y = map(int, xy.split(','))
            pts.append((x + 290, y + 92))
    if len(pts) < 20:
        return None
    xs, ys = sorted(p[0] for p in pts), sorted(p[1] for p in pts)
    return xs[len(xs) // 2], ys[len(ys) // 2]


def run(mode, hold):
    root = work / label / f'{mode}-{hold or "plain"}'
    shutil.rmtree(root, ignore_errors=True)
    (root / 'vault').mkdir(parents=True)
    stub = root / 'stub'
    stub.mkdir()
    opened = root / 'opened.txt'
    (stub / 'xdg-open').write_text(f'#!/bin/sh\nprintf "%s\\n" "$1" >> {opened}\n')
    (stub / 'xdg-open').chmod(0o755)
    note = root / 'vault' / 'links.md'
    note.write_text(NOTE)
    rig = Rig(root, 1366, path_prefix=stub)
    proc = subprocess.Popen([binary, '--vault', str(root / 'vault'), '--index-dir', str(root / 'index'),
                             '--note', 'links.md'], env=rig.env, stdout=open(root / 'app.log', 'w'),
                            stderr=subprocess.STDOUT)
    try:
        time.sleep(8)
        if mode == 'live':
            rig.keys('e', mods=('ctrl', 'shift'))
            time.sleep(2)
            rig.keys('End', mods=('ctrl',))  # caret away from the link
            time.sleep(1)
        shot = rig.shot(root / 'before.png')
        at = link_centre(shot)
        if not at:
            return {'link_at': None}
        rig.click(*at, hold=hold)
        time.sleep(1.5)
        rig.shot(root / 'after.png')
        urls = opened.read_text().splitlines() if opened.exists() else []
        return {'link_at': at, 'opened': urls, 'bytes_kept': note.read_text() == NOTE}
    finally:
        proc.terminate()
        proc.wait(timeout=10)
        rig.close()


results = {'live-logo': run('live', 'logo'), 'live-plain': run('live', None), 'reader-plain': run('reader', None)}
for name, r in results.items():
    print(label, name, r, flush=True)
if any(not r.get('link_at') for r in results.values()):
    print(label, 'INVALID: no link pixels found', flush=True)
    sys.exit(2)
if results['reader-plain']['opened'] != [URL]:
    print(label, 'INVALID: the Reader click handed nothing to the stub, so the stub proves nothing', flush=True)
    sys.exit(2)
checks = {'live_modifier_click_opens': results['live-logo']['opened'] == [URL],
          'live_plain_click_does_not_open': results['live-plain']['opened'] == [],
          'live_bytes_kept': results['live-logo']['bytes_kept'] and results['live-plain']['bytes_kept']}
print(label, checks, flush=True)
sys.exit(0 if all(checks.values()) else 1)
