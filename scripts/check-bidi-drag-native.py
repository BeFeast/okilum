#!/usr/bin/env python3
"""Native #879 regression: mouse drag on a mixed Hebrew/Latin CSV line copies the
highlighted span. Run under an exclusive X11 display with native_bidi737 built.
Usage: DISPLAY=:134 python3 scripts/check-bidi-drag-native.py OUTPUT_DIRECTORY
"""
import json
import math
import os
from pathlib import Path
import subprocess
import sys
import time

out = Path(sys.argv[1])
out.mkdir(parents=True, exist_ok=True)
TEXT = '﻿Name,Count,Description\r\n"שלום, world",12,"change-me"\r\n"line\r\nnext",42,"x"\r\n'
LINE = TEXT.encode().index('"שלום'.encode())
Y = '57'  # second row at Cascadia Code 13
env = dict(os.environ, WAYLAND_DISPLAY='', TESSERA_BIDI_TEXT=TEXT, TESSERA_BIDI_APP='1',
           TESSERA_BIDI_FONT='Cascadia Code', TESSERA_BIDI_SIZE='13',
           __EGL_VENDOR_LIBRARY_FILENAMES='/usr/share/glvnd/egl_vendor.d/50_mesa.json')


def xd(*args):
    subprocess.run(['xdotool', *args], env=env, check=True)
    time.sleep(.3)


for theme in ['light', 'dark']:
    log = out / f'{theme}.jsonl'
    run_env = dict(env, **({'TESSERA_BIDI_DARK': '1'} if theme == 'dark' else {}))
    with log.open('w') as stream:
        proc = subprocess.Popen(['target/debug/examples/native_bidi737'], env=run_env,
                                stdout=stream, stderr=subprocess.STDOUT)
    try:
        for _ in range(40):
            time.sleep(.25)
            found = subprocess.run(['xdotool', 'search', '--onlyvisible', '--pid', str(proc.pid),
                                    '--name', 'Tessera'], env=env, capture_output=True, text=True)
            if found.stdout.strip():
                break
        win = found.stdout.split()[-1]
        xd('windowmove', win, '0', '0', 'windowfocus', win)

        def states():
            return [json.loads(line) for line in log.read_text().splitlines() if line.startswith('{')]

        def record():
            # Wait for the state this keypress requests; the last line may still
            # be the previous one (a stale read looked like a buffer/selection
            # mismatch).
            before = len(states())
            xd('key', 'ctrl+alt+r')
            for _ in range(50):
                current = states()
                if len(current) > before:
                    return current[-1]
                time.sleep(.1)
            raise AssertionError((theme, 'no state recorded'))

        def click(x):
            time.sleep(.6)  # never a double click
            xd('mousemove', str(x), Y, 'click', '1')
            return record()

        def drag(x1, x2):
            time.sleep(.6)
            xd('mousemove', str(x1), Y, 'mousedown', '1')
            xd('mousemove', str((x1 + x2) // 2), Y)
            xd('mousemove', str(x2), Y, 'mouseup', '1')
            state = record()
            r = state['selection']
            selected = TEXT.encode()[r['start']:r['end']].decode()
            # A copy that silently fails must not compare against an older one.
            subprocess.run(['xclip', '-i', '-selection', 'clipboard'], env=env, check=True,
                           input='<stale>', text=True)
            xd('key', 'ctrl+c')
            copied = subprocess.run(['xclip', '-o', '-selection', 'clipboard'], env=env,
                                    capture_output=True, text=True).stdout
            assert copied == selected, (theme, x1, x2, copied, selected)
            return selected

        # Calibrate the LTR run from two caret readouts; Cascadia is monospace.
        a, b = click(100), click(150)
        (o1, x1), (o2, x2) = [(s['cursor'] - LINE, s['caret'][0]) for s in (a, b)]
        assert 11 <= o1 < o2 <= 31, (theme, o1, o2)
        advance = (x2 - x1) / (o2 - o1)

        def x_at(offset):
            return x1 + (offset - o1) * advance

        start = round(x_at(22)) + 1
        # One pixel inside the RTL space whose trailing edge is the end of `change-me`.
        boundary = math.ceil(x_at(31)) + 1
        # Positive control: a plain click there owns the RTL cell (offset 11, before
        # `world`), so a drag that ignored its anchor would copy `world",12,"`.
        assert click(boundary)['cursor'] - LINE == 11, (theme, 'positive control')

        # Pointer jitter after a press on that edge, on either side, selects nothing.
        time.sleep(.6)
        xd('mousemove', str(boundary), Y, 'mousedown', '1')
        for jitter in [boundary + 1, math.floor(x_at(31))]:
            xd('mousemove', str(jitter), Y)
            r = record()['selection']
            assert r['start'] == r['end'], (theme, 'jitter', jitter, r)
        xd('mouseup', '1')

        assert drag(start, boundary) == 'change-me', theme
        subprocess.run(['import', '-window', 'root', str(out / f'{theme}-forward.png')], env=env, check=True)
        assert drag(boundary, start) == 'change-me', theme
        subprocess.run(['import', '-window', 'root', str(out / f'{theme}-reverse.png')], env=env, check=True)
        hebrew = drag(boundary, boundary + 28)
        assert hebrew.endswith(', ') and not any(c.isascii() and c.isalpha() for c in hebrew), (theme, hebrew)
        whole = drag(round(x_at(11)) - 9, boundary)
        assert whole == 'world",12,"change-me"', (theme, whole)
        print(theme, 'PASS', flush=True)
    finally:
        proc.terminate()
        proc.wait(timeout=10)
