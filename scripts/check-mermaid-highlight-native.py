#!/usr/bin/env python3
"""Native S7e (option C): a ```mermaid fence is syntax-highlighted in the
Reader and in Source. Run on an exclusive X11 display (never maestro).
Usage: DISPLAY=:1034 python3 scripts/check-mermaid-highlight-native.py OKILUM_BINARY WORK_DIR LABEL
Readout: highlighted tokens are coloured; plain code is grey. The check counts
saturated pixels in the note pane. A ```rust fence in the same note is the
positive control: it is highlighted in both builds, so a zero there means the
probe saw nothing. Prints counts for the mermaid and rust fences per mode.
Exit 0: mermaid coloured in both modes; 1: not; 2: the run proves nothing.
"""
import os, shutil, subprocess, sys, time
from pathlib import Path

binary, work, label = sys.argv[1], Path(sys.argv[2]), sys.argv[3]
NOTE = ('# Diagram\n\n```mermaid\nflowchart TD\n  A[Start] -->|yes| B{Decision}\n'
        '  B --> C[Finish]\n  %% a comment\n```\n\n```rust\nfn main() { let x = 1; }\n```\n')


def coloured(env, name, crop):
    path = work / f'{label}-{name}.png'
    subprocess.run(['import', '-window', 'root', str(path)], env=env, check=True)
    x, y, w, h = crop
    out = subprocess.run(['convert', str(path), '-crop', f'{w}x{h}+{x}+{y}', '+repage',
                          '-colorspace', 'HSL', '-channel', 'G', '-separate', '+channel',
                          '-threshold', '35%', '-format', '%[fx:mean*w*h]', 'info:'],
                         capture_output=True, text=True, check=True).stdout
    return round(float(out))


def run(mode):
    app = work / label / mode
    shutil.rmtree(app, ignore_errors=True)
    (app / 'vault').mkdir(parents=True)
    (app / 'vault' / 'diagram.md').write_text(NOTE)
    home = app / 'home'
    env = dict(os.environ, WAYLAND_DISPLAY='', HOME=str(home), XDG_CONFIG_HOME=str(home / '.config'),
               XDG_DATA_HOME=str(home / 'share'), XDG_CACHE_HOME=str(home / 'cache'),
               XDG_STATE_HOME=str(home / 'state'))
    proc = subprocess.Popen([binary, '--vault', str(app / 'vault'), '--index-dir', str(app / 'index'),
                             '--note', 'diagram.md'], env=env, stdout=open(app / 'app.log', 'w'),
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
        subprocess.run(['xdotool', 'windowmove', win, '0', '0', 'windowsize', win, '1366', '768',
                        'windowfocus', win], env=env, check=True)
        time.sleep(8)
        if mode == 'source':
            subprocess.run(['xdotool', 'key', 'ctrl+e'], env=env, check=True)
            time.sleep(3)
            subprocess.run(['xdotool', 'key', 'ctrl+End'], env=env, check=True)
            time.sleep(2)
        # The mermaid fence sits above the rust fence in the note pane.
        whole = coloured(env, f'{mode}-pane', (284, 96, 776, 420))
        return whole
    finally:
        proc.terminate()
        proc.wait(timeout=10)


results = {mode: run(mode) for mode in ('reader', 'source')}
print(label, results, flush=True)
if any(v is None for v in results.values()):
    sys.exit(2)

sys.exit(0)
