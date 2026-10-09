#!/usr/bin/env python3
"""Native #737 regression: run under an exclusive X11 display with native_bidi737 built.
Usage: DISPLAY=:134 [BIDI_STEP_DELAY=0.5] python3 scripts/check-bidi-native.py OUTPUT_DIRECTORY
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import time

out = Path(sys.argv[1])
out.mkdir(parents=True, exist_ok=True)
env = dict(os.environ, WAYLAND_DISPLAY='', __EGL_VENDOR_LIBRARY_FILENAMES='/usr/share/glvnd/egl_vendor.d/50_mesa.json')

# Settle time per xdotool step; raise it on a loaded host (BIDI_STEP_DELAY=0.5).
STEP = float(os.environ.get('BIDI_STEP_DELAY', '.2'))

def xd(*args):
    subprocess.run(['xdotool', *args], env=env, check=True)
    time.sleep(STEP)

for live in [False, True]:
    for kind, text in [('plain', 'abc שלום xyz'), ('bold', 'abc **שלום** xyz'), ('wiki', 'abc [[שלום]] xyz'), ('wrap', 'abc שלום xyz ' * 35)]:
        if not live and kind in ['bold', 'wiki']:
            continue
        name = f'{kind}-{live}'
        log = out / f'{name}.jsonl'
        with log.open('w') as stream:
            proc = subprocess.Popen(['target/debug/examples/native_bidi737'], env=dict(env, TESSERA_BIDI_TEXT=text), stdout=stream, stderr=subprocess.STDOUT)
        try:
            # Wait until the window is mapped; focusing an unmapped window is BadMatch.
            for _ in range(40):
                time.sleep(.5)
                found = subprocess.run(['xdotool', 'search', '--onlyvisible', '--pid', str(proc.pid), '--name', 'Tessera'], env=env, text=True, capture_output=True).stdout.split()
                if found:
                    break
            assert found, f'{name}: window was not mapped within 20 s'
            win = found[-1]
            xd('windowmove', win, '0', '0')
            xd('windowfocus', '--sync', win)
            if live:
                xd('key', 'ctrl+alt+l')
            def records():
                return [json.loads(line) for line in log.read_text().splitlines() if line.startswith('{')]
            def record():
                # The fixture writes asynchronously; wait for this request's line.
                count = len(records())
                xd('key', 'ctrl+alt+r')
                for _ in range(50):
                    if len(records()) > count:
                        break
                    time.sleep(.05)
                return records()[-1]
            def selected(state):
                r = state['selection']
                return text.encode()[r['start']:r['end']].decode()
            xd('key', 'ctrl+Home')
            if kind == 'wrap':
                xd('key', 'Down')
                before = record()['cursor']
                xd('key', 'Home')
                after = record()['cursor']
                assert 0 < after <= before, (name, before, after)
                xd('key', 'Home')
                assert record()['cursor'] == after, 'Home stays on its visual row'
            elif kind == 'plain' or live:
                xd('key', '--repeat', '6' if kind in ['bold', 'wiki'] else '4', 'Right')
                xd('key', 'shift+Right')
                assert selected(record()) == 'ם', (name, record())
                xd('key', 'ctrl+Home', 'End')
                xd('key', '--repeat', '6' if kind in ['bold', 'wiki'] else '4', 'Left')
                xd('key', 'shift+Left')
                assert selected(record()) == 'ש', (name, record())
                if kind == 'plain':
                    for x in ['110', '90', '136']:
                        time.sleep(.6)
                        xd('mousemove', x, '43', 'click', '--repeat', '2', '--delay', '70', '1')
                        assert selected(record()) == 'שלום', (name, x, record())
            subprocess.run(['import', '-window', 'root', str(out / f'{name}.png')], env=env, check=True)
            print(name, 'PASS', flush=True)
        finally:
            proc.terminate()
            proc.wait(timeout=10)
