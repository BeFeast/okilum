#!/usr/bin/env python3
"""Home/End regression on painted rows, including a wrap inside Hebrew (#780).

Run on an exclusive X11 display (Xvfb supported) with Noto Sans installed:
DISPLAY=:138 python3 scripts/check-bidi-row-boundaries-native.py OUTPUT [BINARY]
The main-build binary is a positive control: fails the repeated End / row-boundary checks.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import time

out = Path(sys.argv[1])
out.mkdir(parents=True, exist_ok=True)
binary = sys.argv[2] if len(sys.argv) > 2 else 'target/debug/examples/native_bidi737'
text = 'word00 ' * 14 + 'שלום ' + ' '.join(f'word{i:02}' for i in range(85))
# Formatting below the measured paragraph makes LP non-identity without changing
# its wrap fixture. No user files are opened by the native example.
text += '\n\n**projected**'
source = text.encode()
results = []
for live in [False, True]:
    for dark in [False, True]:
        name = f'{"live" if live else "source"}-{"dark" if dark else "light"}'
        env = dict(os.environ, WAYLAND_DISPLAY='', TESSERA_BIDI_TEXT=text,
                   __EGL_VENDOR_LIBRARY_FILENAMES='/usr/share/glvnd/egl_vendor.d/50_mesa.json')
        for key, enabled in [('TESSERA_BIDI_LIVE', live), ('TESSERA_BIDI_DARK', dark)]:
            env.pop(key, None)
            if enabled:
                env[key] = '1'
        log = out / f'{name}.jsonl'
        with log.open('w') as stream:
            proc = subprocess.Popen([binary], env=env, stdout=stream, stderr=subprocess.STDOUT)
        def xd(*args):
            return subprocess.check_output(['xdotool', *map(str, args)], env=env, text=True).strip()
        def key(value):
            xd('key', value)
            time.sleep(.12)
        def record():
            key('ctrl+alt+r')
            return [json.loads(line) for line in log.read_text().splitlines() if line.startswith('{')][-1]
        def screen_y(state):
            return round(state['caret'][1] - state['scroll'][1], 2)
        try:
            time.sleep(2)
            win = xd('search', '--pid', proc.pid, '--name', 'Tessera').splitlines()[-1]
            xd('windowmove', win, 0, 0, 'windowfocus', win)
            for width in [500, 800, 1100]:
                xd('windowsize', win, width, 700)
                time.sleep(.3)
                key('ctrl+Home')
                origin = record()
                y, height = origin['caret'][1:]
                previous_end = None
                for row in range(5):
                    xd('mousemove', 100, y + row * height + 5, 'click', 1)
                    time.sleep(.15)
                    before = record()
                    key('Home')
                    home = record()
                    assert screen_y(home) == screen_y(before), (name, width, row, 'Home Y')
                    if previous_end is not None:
                        assert home['cursor'] == previous_end, (name, width, row, 'row continuity', home['cursor'], previous_end)
                    key('Home')
                    assert record()['cursor'] == home['cursor'], 'Repeated Home changed row'
                    key('End')
                    end = record()
                    assert screen_y(end) == screen_y(before), (name, width, row, 'End Y')
                    assert end['cursor'] > home['cursor'], (name, width, row, 'logical bounds')
                    key('End')
                    assert record()['cursor'] == end['cursor'], 'Repeated End changed row'
                    previous_end = end['cursor']
                    key('Home')
                    xd('type', 'X')
                    time.sleep(.15)
                    inserted = record()
                    offset = home['cursor']
                    assert inserted['source'].encode() == source[:offset] + b'X' + source[offset:], 'Insertion at wrong source boundary'
                    # Editing changes wrap opportunities: an inserted Latin glyph before
                    # Hebrew may fit on the preceding row. Source position, not edit Y,
                    # is the invariant; navigation Y is checked before any edit.
                    key('ctrl+z')
                    assert record()['source'] == text, 'Undo must restore exact source'
                    results.append(dict(mode=name, width=width, row=row, start=offset, end=end['cursor'], y=screen_y(home)))
                subprocess.run(['import', '-window', win, str(out / f'{name}-{width}.png')], env=env, check=True)
                print(name, width, 'PASS', flush=True)
        finally:
            proc.terminate()
            proc.wait(timeout=10)
(out / 'results.json').write_text(json.dumps(results, indent=2) + '\n')
