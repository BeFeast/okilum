#!/usr/bin/env python3
"""Native screenshots of the start screen's recent vaults (#3208).

Seeds the runner's own Reader state (never `OKILUM_STATE_DIR`: only the
user's own history drops folders nobody chose as a vault) with the kind of
list Oleg reported: a dozen real vaults, one with a long path, QA fixtures,
an app-support folder with a UUID name, a temporary folder, a folder that
only held a file opened on its own, a folder that is gone, and the newest
vault on a volume that is not mounted, so startup cannot restore it and
shows the start screen, plus an unsaved untitled draft. Runs the real
`okilum` binary once per state and captures the screen after it has drawn:
the long list at the default and a narrow window size, the filter, held ⌘
(Ctrl) numbers, Clear list confirmation (through the non-publishing
`welcome-ui-harness`, `OKILUM_WELCOME_HARNESS`), and first launch with no
history. The state file after each run is kept next to the capture.

Hosted runners are throwaway machines: the seeded folders live under the
runner's home and the Reader state is the runner's own.

Usage: welcome-native-screens.py <binary> <output-dir>
"""
import json
import os
from pathlib import Path
import platform
import shutil
import string
import subprocess
import sys
import tempfile
import time

SETTLE_S = float(os.environ.get('WELCOME_SCREENS_SETTLE_S', '15'))
SYSTEM = platform.system()


def capture(path):
    if SYSTEM == 'Darwin':
        subprocess.run(['screencapture', '-x', '-t', 'png', str(path)], check=True, timeout=60)
    elif SYSTEM == 'Windows':
        script = (
            'Add-Type -AssemblyName System.Windows.Forms,System.Drawing;'
            '$b=[System.Windows.Forms.Screen]::PrimaryScreen.Bounds;'
            '$i=New-Object System.Drawing.Bitmap($b.Width,$b.Height);'
            '$g=[System.Drawing.Graphics]::FromImage($i);'
            '$g.CopyFromScreen($b.Location,[System.Drawing.Point]::Empty,$b.Size);'
            f'$i.Save("{path}",[System.Drawing.Imaging.ImageFormat]::Png)'
        )
        subprocess.run(['powershell.exe', '-NoProfile', '-NonInteractive', '-Command', script],
                       check=True, timeout=60)
    else:
        subprocess.run(['import', '-window', 'root', str(path)], check=True, timeout=60)


def check_image(path):
    from PIL import Image
    with Image.open(path) as image:
        extrema = image.convert('L').getextrema()
    assert extrema[0] != extrema[1], f'{path} is a single colour {extrema}'


def state_directory():
    """The Reader's own state directory, as `reader_history::state_directory`."""
    home = Path.home()
    if SYSTEM == 'Darwin':
        return home / 'Library' / 'Application Support' / 'com.befeast.okilum'
    if SYSTEM == 'Windows':
        return Path(os.environ['LOCALAPPDATA']) / 'okilum'
    xdg = os.environ.get('XDG_STATE_HOME')
    base = Path(xdg) if xdg and Path(xdg).is_absolute() else home / '.local' / 'state'
    return base / 'okilum'


def unmounted_volume():
    """A vault on a volume that is not there on this runner."""
    if SYSTEM == 'Darwin':
        return Path('/Volumes/Maestro/src/_DropBox/okilum/theme-showcase')
    if SYSTEM == 'Windows':
        letter = next(l for l in reversed(string.ascii_uppercase[3:])
                      if not Path(f'{l}:\\').exists())
        return Path(f'{letter}:\\okilum\\theme-showcase')
    return Path('/mnt/Maestro/okilum/theme-showcase')


def seed(work):
    home = Path.home()
    demo = home / 'welcome-demo'
    vaults = [
        demo / 'vaults' / 'brain',
        demo / 'Developer' / 'personal' / 'okilum-log-samples',
        demo / 'Developer' / 'work' / 'vault',
        demo / 'Developer' / 'engineering' / 'runs' / '2026-11-10' / 'oncall-kt',
        demo / 'Developer' / 'personal' / 'projects' / '2026' / 'okilum'
        / 'a-really-long-folder-name-that-checks-middle-truncation-of-the-path' / 'Engineering Notes',
        demo / 'Documents' / 'Recipes',
        demo / 'Documents' / 'Travel 2026',
        demo / 'Documents' / 'Reading list',
        demo / 'Notes' / 'Journal',
        demo / 'Notes' / 'Garden',
        demo / 'Notes' / 'Home lab',
        demo / 'Notes' / 'Music',
    ]
    junk = [
        demo / 'work' / 'qa-evidence' / 'okilum-manager-panels-2693-2694' / 'normal-install' / 'fixture-a',
        demo / 'work' / 'qa-evidence' / 'okilum-manager-panels-2693-2694' / 'normal-install' / 'fixture-b',
        demo / 'work' / 'qa-evidence' / 'okilum-sync-showstopper-1108' / 'production-fix-vault',
        demo / 'Library' / 'Application Support' / 'com.example.Recorder' / 'Meetings'
        / 'D8B1ABFA-1731-48C9-BC91-259810B1639C',
        Path(os.path.realpath(tempfile.mkdtemp(prefix='okilum-fixture-'))),
    ]
    quick = demo / 'Downloads'
    missing = demo / 'Developer' / 'old-notes'
    for folder in vaults + junk:
        folder.mkdir(parents=True, exist_ok=True)
        (folder / 'Welcome.md').write_text('# Welcome\n', encoding='utf-8', newline='\n')
    quick.mkdir(parents=True, exist_ok=True)
    (quick / 'readme.md').write_text('# Readme\n', encoding='utf-8', newline='\n')
    shutil.rmtree(missing, ignore_errors=True)
    unavailable = unmounted_volume()
    # Newest first: the unmounted vault is the last one used, so startup
    # cannot restore it and shows the start screen; junk is interleaved.
    recent = [unavailable, vaults[0], junk[0], vaults[1], junk[3], vaults[2], quick, vaults[3],
              junk[1], vaults[4], missing, junk[4], *vaults[5:], junk[2]]
    documents = {str(root): 'Welcome.md' for root in recent}
    documents[str(quick)] = 'readme.md'
    history = {
        'schema': 1,
        'last_documents': documents,
        'quick_roots': [str(quick)],
        'recent_roots': [str(root) for root in recent],
        # theme-showcase was on a network volume when last opened.
        'network_roots': [str(unavailable)],
        'last_opened': {str(root): int(time.time()) - 3600 * 26 * i
                        for i, root in enumerate(vaults)},
    }
    return history


def seed_draft(state):
    """An unsaved untitled draft (#1528), as New file leaves one."""
    folder = state / 'untitled' / '0f6c7a52-2d0e-4f7e-9d43-3b1f0c9a8e11'
    folder.mkdir(parents=True)
    (folder / 'draft.json').write_text(json.dumps({'origin': None, 'created': int(time.time())}),
                                       encoding='utf-8')
    (folder / 'Untitled.md').write_text('# Meeting notes\n\nAgenda\n', encoding='utf-8',
                                        newline='\n')


def launch(binary, out, label, history, frame, harness=None, draft=True):
    state = state_directory()
    shutil.rmtree(state, ignore_errors=True)
    state.mkdir(parents=True)
    if history is not None:
        (state / 'update-session.json').write_text(json.dumps(history, indent=1),
                                                   encoding='utf-8')
        (out / f'{label}-state-before.json').write_text(json.dumps(history, indent=1),
                                                        encoding='utf-8')
    if draft:
        seed_draft(state)
    if frame:
        (state / 'window-frames.json').write_text(json.dumps({'frames': {'entry#0': frame}}),
                                                  encoding='utf-8')
    env = {k: v for k, v in os.environ.items() if not k.startswith('OKILUM_')}
    if harness:
        env['OKILUM_WELCOME_HARNESS'] = harness
    work = Path(tempfile.mkdtemp(prefix=f'welcome-{label}-'))
    log = open(out / f'{label}.log', 'w', encoding='utf-8')
    proc = subprocess.Popen([binary, '--index-dir', str(work / 'index')],
                            env=env, stdout=log, stderr=subprocess.STDOUT)
    try:
        time.sleep(SETTLE_S)
        if proc.poll() is not None:
            raise SystemExit(f'{label}: okilum exited with {proc.returncode} (see {label}.log)')
        path = out / f'{label}.png'
        capture(path)
        check_image(path)
        print(label, 'captured', flush=True)
    finally:
        proc.terminate()
        try:
            proc.wait(10)
        except subprocess.TimeoutExpired:
            proc.kill()
        log.close()
    after = state / 'update-session.json'
    if after.exists():
        shutil.copy(after, out / f'{label}-state-after.json')


def main():
    binary, out = sys.argv[1], Path(sys.argv[2])
    out.mkdir(parents=True, exist_ok=True)
    work = Path(tempfile.mkdtemp(prefix='welcome-seed-'))
    history = seed(work)
    only = set(filter(None, os.environ.get('WELCOME_SCREENS_ONLY', '').split(',')))
    narrow = {'x': 40.0, 'y': 40.0, 'width': 440.0, 'height': 820.0, 'display': None,
              'fullscreen': False, 'maximized': False}
    runs = [
        ('default', history, None, None, True),
        ('narrow', history, narrow, None, True),
        ('filter', history, None, 'query=work', True),
        ('numbers', history, None, 'numbers', True),
        ('clear-list', history, None, 'confirm_clear', True),
        ('keyboard', history, None, 'keyboard;selected=3', True),
        ('first-launch', None, None, None, False),
    ]
    for label, seeded, frame, harness, draft in runs:
        if only and label not in only:
            continue
        launch(binary, out, label, seeded, frame, harness, draft)


if __name__ == '__main__':
    main()
