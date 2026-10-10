#!/usr/bin/env python3
"""Native #1126: in a fresh vault and profile, the first note's local PNG is
drawn without navigating away and back.
Run on an exclusive X11 display (never maestro).
Usage: DISPLAY=:1034 python3 scripts/check-first-open-image-native.py OKILUM_BINARY WORK_DIR LABEL
The vault (no index, no profile) holds Recovery.md with `![Blue](blue.png)`, a
solid 420x100 #3366cc PNG, and Other.md. Open Recovery in the Reader, wait
5 s, count image pixels. Then follow Other and come Back, and count again:
that second count is the positive control (the image can be drawn at all).
Exit 0: drawn on first opening; 1: only after navigating; 2: the run proves
nothing (no window, or not drawn even after navigating).
"""
import os, shutil, struct, subprocess, sys, time, zlib
from pathlib import Path

binary, work, label = sys.argv[1], Path(sys.argv[2]), sys.argv[3]
CROP = (284, 96, 776, 560)


def png(path, rgb, w=420, h=100):
    raw = b''.join(b'\x00' + bytes(rgb) * w for _ in range(h))
    chunk = lambda tag, data: (struct.pack('>I', len(data)) + tag + data
                               + struct.pack('>I', zlib.crc32(tag + data) & 0xffffffff))
    path.write_bytes(b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 2, 0, 0, 0))
                     + chunk(b'IDAT', zlib.compress(raw)) + chunk(b'IEND', b''))


def xd(env, *args):
    subprocess.run(['xdotool', *args], env=env, check=True)
    time.sleep(.3)


def blue(env, name):
    path = work / f'{label}-{name}.png'
    subprocess.run(['import', '-window', 'root', str(path)], env=env, check=True)
    x, y, w, h = CROP
    out = subprocess.run(['convert', str(path), '-crop', f'{w}x{h}+{x}+{y}', '+repage',
                          '-fuzz', '6%', '-fill', 'white', '+opaque', '#3366cc',
                          '-fill', 'black', '-opaque', '#3366cc', '-format', '%[fx:mean*w*h]',
                          'info:'], capture_output=True, text=True, check=True).stdout
    return w * h - round(float(out))


app = work / label
shutil.rmtree(app, ignore_errors=True)
vault = app / 'vault'
vault.mkdir(parents=True)
(vault / 'Recovery.md').write_text('# Recovery\n\nA paragraph.\n\n![Blue](blue.png)\n\n[[Other]]\n')
(vault / 'Other.md').write_text('# Other\n\n[[Recovery]]\n')
png(vault / 'blue.png', (0x33, 0x66, 0xcc))
home = app / 'home'
env = dict(os.environ, WAYLAND_DISPLAY='', HOME=str(home), XDG_CONFIG_HOME=str(home / '.config'),
           XDG_DATA_HOME=str(home / 'share'), XDG_CACHE_HOME=str(home / 'cache'),
           XDG_STATE_HOME=str(home / 'state'))
proc = subprocess.Popen([binary, '--vault', str(vault), '--index-dir', str(app / 'index'),
                         '--note', 'Recovery.md'], env=env, stdout=open(app / 'app.log', 'w'),
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
    xd(env, 'windowmove', win, '0', '0', 'windowsize', win, '1366', '768', 'windowfocus', win)
    time.sleep(5)
    first = blue(env, '1-first')
    # Positive control: Other, then Back (Alt+Left).
    xd(env, 'key', 'ctrl+k')  # Quick Open
    time.sleep(.5)
    xd(env, 'type', 'Other')
    time.sleep(.8)
    xd(env, 'key', 'Return')
    time.sleep(1.5)
    xd(env, 'key', 'alt+Left')
    time.sleep(2)
    after = blue(env, '2-after-back')
    print(label, {'first_open': first, 'after_back': after}, flush=True)
    if after < 20000:
        print(label, 'INVALID run: the image is not drawn even after navigating', flush=True)
        sys.exit(2)
    sys.exit(0 if first >= 20000 else 1)
finally:
    proc.terminate()
    proc.wait(timeout=10)
