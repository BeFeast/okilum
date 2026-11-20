#!/usr/bin/env python3
"""Native screenshots of the theme decoration slots (#3154).

Runs the real `okilum` binary on a synthetic note that uses all five slots
(heading rule, hr, blockquote, link marker, bold gradient), once per theme,
mode and view: Reader, and Live Preview restored from `reader-ui.json` (no
input automation). Each launch has its own OKILUM_STATE_DIR. The default
theme, which fills no slot, is the control. The screen is captured after the
window has had time to draw; each capture must show more than one colour.

Usage: theme-native-screens.py <binary> <output-dir>
"""
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import tempfile
import time

NOTE = """# Gradient slots

Plain text with **a bold phrase in the gradient** and a [[Other note]] link, plus an [external link](https://example.com) in the same line, long enough to wrap onto a second row in the reading column.

## Heading two carries the rule

> A blockquote with an angled background,
> a left border and short bars at the top and bottom.

---

### Heading three

Closing paragraph with **bold** text and *italic* text.
"""

RUNS = [('obsidianite', 'dark'), ('obsidianite', 'light'), ('okilum', 'dark')]
SETTLE_S = float(os.environ.get('THEME_SCREENS_SETTLE_S', '15'))


def capture(path):
    system = platform.system()
    if system == 'Darwin':
        subprocess.run(['screencapture', '-x', '-t', 'png', str(path)], check=True, timeout=60)
    elif system == 'Windows':
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


def canonical_root(path):
    """The vault key `reader-ui.json` uses: the canonical path (verbatim on Windows)."""
    root = str(Path(path).resolve())
    if platform.system() == 'Windows' and not root.startswith('\\\\?\\'):
        root = '\\\\?\\' + root
    return root


def launch(binary, out, theme, mode, live):
    label = f"{theme}-{mode}-{'live' if live else 'reader'}"
    work = Path(tempfile.mkdtemp(prefix=f'theme-{label}-'))
    vault, state = work / 'vault', work / 'state'
    vault.mkdir()
    state.mkdir()
    # LF on every platform: Windows text mode would write CRLF.
    (vault / 'demo.md').write_text(NOTE, encoding='utf-8', newline='\n')
    (vault / 'Other note.md').write_text('# Other note\n', encoding='utf-8', newline='\n')
    (state / 'reader-ui.json').write_text(json.dumps({
        'version': 1,
        'theme': theme,
        'appearance': mode,
        'vaults': {canonical_root(vault): {'note': 'demo.md', 'source': live, 'live_preview': live}},
    }), encoding='utf-8')
    env = {k: v for k, v in os.environ.items() if not k.startswith('OKILUM_')}
    env['OKILUM_STATE_DIR'] = str(state)
    log = open(out / f'{label}.log', 'w', encoding='utf-8')
    proc = subprocess.Popen([binary, '--vault', str(vault), '--index-dir', str(work / 'index')],
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


def main():
    binary, out = sys.argv[1], Path(sys.argv[2])
    out.mkdir(parents=True, exist_ok=True)
    for theme, mode in RUNS:
        for live in (False, True):
            launch(binary, out, theme, mode, live)


if __name__ == '__main__':
    main()
