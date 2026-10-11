#!/usr/bin/env python3
"""Native #859: rapid Home/End in Live Preview. The source offset and the drawn
caret must agree, and a character typed right after End lands at the caret.
Run on an exclusive X11 display (Xvfb supported) with Noto Sans installed:
DISPLAY=:1034 python3 scripts/check-rapid-home-end-native.py OUTPUT [BINARY]
The issue's fixture and cadence: Noto Sans 24, widths 760/800, rows 0-2, three
repeats, light/dark, Live Preview and Source; 80 ms after each key. Per row:
Ctrl+Home, Down x row, Home, Home, End, End (a record after each), then X is
typed directly after the second End, and Undo.
Each record carries `cursor` (state) and `painted_cursor` (the offset the drawn
caret was laid out for). Only a record whose two offsets agree describes one
paint; a mixed record is counted and read again on the next frame.
Positive control: the checks must reject a stale record (the state from before
the key), so a pass is not a detector that never fires.
Exit 0: all hold; 1: a check failed; 2: the run proves nothing.
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
CADENCE = .08
text = ('word00 ' * 14 + 'שלום ' + ' '.join(f'word{i:02}' for i in range(85))
        + '\n\n**projected**')
source = text.encode()


def checks(home, home2, end, end2):
    """Failed check names for one row; empty when the row is consistent."""
    failed = []
    if home2['cursor'] != home['cursor']:
        failed.append('repeated Home moved')
    if end2['cursor'] != end['cursor']:
        failed.append('repeated End moved')
    if not end['cursor'] > home['cursor']:
        failed.append('End not after Home')
    if round(end['caret'][1], 1) != round(home['caret'][1], 1):
        failed.append('End caret on another row')
    return failed


results, mixed, failures = [], 0, []
for live in (True, False):
    for dark in (False, True):
        name = f'{"live" if live else "source"}-{"dark" if dark else "light"}'
        env = dict(os.environ, WAYLAND_DISPLAY='', OKILUM_BIDI_TEXT=text, OKILUM_BIDI_SIZE='24',
                   __EGL_VENDOR_LIBRARY_FILENAMES='/usr/share/glvnd/egl_vendor.d/50_mesa.json')
        for key_name, enabled in [('OKILUM_BIDI_LIVE', live), ('OKILUM_BIDI_DARK', dark)]:
            env.pop(key_name, None)
            if enabled:
                env[key_name] = '1'
        log = out / f'{name}.jsonl'
        with log.open('w') as stream:
            proc = subprocess.Popen([binary], env=env, stdout=stream, stderr=subprocess.STDOUT)

        def xd(*args):
            return subprocess.check_output(['xdotool', *map(str, args)], env=env, text=True).strip()

        def key(value):
            xd('key', value)
            time.sleep(CADENCE)

        def lines():
            return [json.loads(l) for l in log.read_text().splitlines() if l.startswith('{')]

        def record():
            """The next record that describes one paint."""
            global mixed
            for _ in range(20):
                count = len(lines())
                xd('key', 'ctrl+alt+r')
                for _ in range(100):
                    if len(lines()) > count:
                        break
                    time.sleep(.01)
                state = lines()[-1]
                if state['painted_cursor'] == state['cursor'] and state['caret']:
                    return state
                mixed += 1
                time.sleep(.016)
            raise AssertionError(f'{name}: no single-paint record in 20 tries')

        try:
            found = ''
            for _ in range(40):
                time.sleep(.5)
                found = subprocess.run(['xdotool', 'search', '--onlyvisible', '--pid', str(proc.pid),
                                        '--name', 'Okilum'], env=env, text=True,
                                       capture_output=True).stdout.split()
                if found:
                    break
            assert found, f'{name}: window was not mapped within 20 s'
            win = found[-1]
            xd('windowmove', win, 0, 0)
            xd('windowfocus', '--sync', win)
            for width in (760, 800):
                xd('windowsize', win, width, 700)
                time.sleep(.5)
                for repeat in range(3):
                    for row in range(3):
                        key('ctrl+Home')
                        for _ in range(row):
                            key('Down')
                        before = record()
                        key('Home')
                        home = record()
                        key('Home')
                        home2 = record()
                        key('End')
                        end = record()
                        key('End')
                        end2 = record()
                        failed = checks(home, home2, end, end2)
                        # Positive control: the record from before End, taken as End.
                        if not checks(home, home2, home2, home2):
                            failed.append('control: a stale record passed')
                        # X directly after the second End, no reset: it lands at the caret.
                        xd('type', 'X')
                        time.sleep(CADENCE)
                        typed = record()
                        at = end2['cursor']
                        if typed['source'].encode() != source[:at] + b'X' + source[at:]:
                            failed.append(f'X not at the caret {at}')
                        key('ctrl+z')
                        if record()['source'] != text:
                            failed.append('Undo not exact')
                        case = dict(mode=name, width=width, repeat=repeat, row=row, home=home['cursor'],
                                    end=end['cursor'], y=home['caret'][1], failed=failed)
                        results.append(case)
                        if failed:
                            failures.append(case)
                            subprocess.run(['import', '-window', win, str(out / f'{name}-{width}-{repeat}-{row}.png')],
                                           env=env, check=True)
                subprocess.run(['import', '-window', win, str(out / f'{name}-{width}.png')], env=env, check=True)
            print(name, 'cases', sum(r['mode'] == name for r in results), 'failed',
                  sum(1 for f in failures if f['mode'] == name), flush=True)
        finally:
            proc.terminate()
            proc.wait(timeout=10)

(out / 'results.json').write_text(json.dumps(dict(results=results, mixed_records=mixed), indent=2) + '\n')
print('cases', len(results), 'failed', len(failures), 'mixed records re-read', mixed, flush=True)
for case in failures:
    print('FAIL', case, flush=True)
if len(results) != 48:
    sys.exit(2)
sys.exit(1 if failures else 0)
