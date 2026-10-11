#!/usr/bin/env python3
"""Native #1236: input-method composition in Live Preview, through Wayland the
way Oleg's Hyprland runs it (gpui text-input-v3 <-> fcitx5 input-method-v2).
Runs its own headless sway, so nothing reaches a real screen; never on maestro.
Needs: sway, wtype, grim, fcitx5 with fcitx5-chinese-addons (pinyin) and
fcitx5-mozc, gdbus, dbus-launch, ImageMagick, fonts-noto-cjk.
Usage: python3 scripts/check-ime-native.py OKILUM_BINARY WORK_DIR LABEL

The note is the L1 IME fixture. For each «type here» spot (a list item, a
quote, right after a **bold** run) and each composing input method (Pinyin
`nihao` -> 你好, Mozc `nihongo` -> 日本語), in Live Preview:
- the preedit and the candidate window appear, the window just under the
  preedit, and nothing else on screen changes (no row reveals or moves);
- Escape cancels the composition: the saved bytes are the original;
- the commit inserts the text exactly once inside the spot's line, and one
  Undo restores the original bytes.
Hebrew, Russian and a Latin `x` typed directly through the same grab are the
controls: no candidate window, the text lands as typed, one Undo restores it.
A composition may change no more of the screen than typing `x` at the same spot
(an edited block is redrawn as Markdown until it is classified again). Light and dark, at 1366 and 700 px wide.
Positive controls: the candidate window follows the caret (its top on the
after-bold spot is below the list spot's), and the screen diff sees the preedit.
Known: #1241 scrolls the column when the quote line is edited on Wayland; the
screen checks are skipped there (reported as scrolled_1241), the byte checks run.
Exit 0: all hold; 1: a check failed; 2: the run proves nothing.
"""
import json, os, shutil, subprocess, sys, time
from pathlib import Path

from wayland_rig import Rig

binary, work, label = sys.argv[1], Path(sys.argv[2]), sys.argv[3]
NOTE = ('# IME\n\n- list item: type here →\n> quote: type here →\n'
        'Paragraph with **bold** then type here →\n| heading | after |\n')
TAIL = ' then type here →'
# (name, line index, keys after End to reach the spot)
SPOTS = [('list', 2, 0), ('quote', 3, 0), ('after-bold', 4, len(TAIL))]
COMPOSERS = [('pinyin', 'nihao', ['space'], '你好'), ('mozc', 'nihongo', ['space', 'Return'], '日本語')]
# Direct input through the same input-method grab, no composition (controls).
DIRECT = [('hebrew', 'שלום'), ('russian', 'привет'), ('latin', 'x')]
EDITOR = (290, 92, 1080, 768)  # the note column, below the toolbar, at 1366
POPUP = 'srgb(192,192,192)'  # fcitx5's classic candidate window border


def popup(path, width):
    """Bounding box of the candidate window's border inside the note column, or None."""
    x0, y0, x1, y1 = EDITOR if width > 1000 else (8, 92, width - 8, 768)
    out = subprocess.run(['convert', str(path), '-crop', f'{x1 - x0}x{y1 - y0}+{x0}+{y0}', '+repage',
                          '-fill', 'black', '-opaque', POPUP, '-fill', 'white', '+opaque', 'black',
                          '-negate', '-trim', 'info:'], capture_output=True, text=True).stdout.split()
    # "info:" prints e.g. "- PNG 330x44 790x676+229+101 ..." for the trimmed box.
    if len(out) < 4 or out[2] in ('1x1',):
        return None
    w, h = map(int, out[2].split('x'))
    _, ox, oy = out[3].split('+')
    if w < 40 or h < 15:
        return None
    return [x0 + int(ox), y0 + int(oy), x0 + int(ox) + w, y0 + int(oy) + h]


def changes(a, b, width, mask=None):
    """Bounding boxes [x0, y0, x1, y1] of changes inside the note column (the toolbar's
    unsaved dot excluded), caret-sized blobs dropped, `mask` painted out first."""
    x0, y0, x1, y1 = EDITOR if width > 1000 else (8, 92, width - 8, 768)
    draw = [] if not mask else ['-fill', 'white', '-draw', f'rectangle {mask[0] - 2},{mask[1] - 2} {mask[2] + 2},{mask[3] + 2}']
    out = subprocess.run(['convert', '(', str(a), *draw, ')', '(', str(b), *draw, ')',
                          '-compose', 'difference', '-composite', '-alpha', 'off',
                          '-crop', f'{x1 - x0}x{y1 - y0}+{x0}+{y0}', '+repage',
                          '-colorspace', 'gray', '-threshold', '12%', '-morphology', 'Dilate', 'Square:3',
                          '-define', 'connected-components:verbose=true',
                          '-define', 'connected-components:area-threshold=30',
                          '-connected-components', '8', 'null:'], capture_output=True, text=True).stdout
    boxes = []
    for line in out.splitlines()[1:]:
        parts = line.split()
        if len(parts) < 5 or not parts[-1].startswith(('gray(255', 'graya(255', 'srgb(255')):
            continue
        size, x, y = parts[1].split('+')  # WxH+X+Y
        w, h = map(int, size.split('x'))
        x, y = int(x) + x0, int(y) + y0
        if w <= 4:  # the caret blinking
            continue
        boxes.append([x, y, x + w, y + h])
    # Merge boxes that overlap vertically into clusters (a text row, the candidate window).
    boxes.sort(key=lambda b: b[1])
    merged = []
    for b in boxes:
        if merged and b[1] <= merged[-1][3] + 2:
            m = merged[-1]
            merged[-1] = [min(m[0], b[0]), min(m[1], b[1]), max(m[2], b[2]), max(m[3], b[3])]
        else:
            merged.append(b)
    return merged


def launch(rig, app, dark):
    shutil.rmtree(app, ignore_errors=True)
    (app / 'vault').mkdir(parents=True)
    note = app / 'vault' / 'l1-ime.md'
    note.write_bytes(NOTE.encode())
    conf = Path(rig.env['XDG_CONFIG_HOME']) / 'okilum'
    conf.mkdir(parents=True, exist_ok=True)
    (conf / 'appearance.json').write_text(json.dumps({'appearance': 'dark' if dark else 'light'}))
    proc = subprocess.Popen([binary, '--vault', str(app / 'vault'), '--index-dir', str(app / 'index'),
                             '--note', 'l1-ime.md'], env=rig.env, stdout=open(app / 'app.log', 'w'),
                            stderr=subprocess.STDOUT)
    time.sleep(8)
    rig.keys('e', mods=('ctrl', 'shift'))  # Live Preview
    time.sleep(2)
    return proc, note


def goto(rig, line, back):
    rig.keys('Home', mods=('ctrl',))
    for _ in range(line):
        rig.keys('Down')
    rig.keys('End')
    for _ in range(back):
        rig.keys('Left')
    time.sleep(.5)


def save(rig, note):
    time.sleep(.8)  # let the last key land before saving
    rig.keys('s', mods=('ctrl',))
    time.sleep(1.2)
    return note.read_bytes().decode()


def restore(rig, note):
    """Undo until the saved note is the original again; the steps it took (0 = exact)."""
    for steps in range(5):
        if save(rig, note) == NOTE:
            return steps
        rig.keys('z', mods=('ctrl',))
    return None


def run(theme, width):
    root = work / label / f'{theme}-{width}'
    shutil.rmtree(root, ignore_errors=True)
    root.mkdir(parents=True)
    rig = Rig(root, width, [c[0] for c in COMPOSERS])
    proc = None
    results, failed = [], []
    try:
        proc, note = launch(rig, root / 'app', theme == 'dark')
        if note.read_bytes().decode() != NOTE:
            return None, ['fixture changed on open']
        control = {}
        for spot, line, back in SPOTS:
            for name, text in DIRECT:
                tag = f'{spot}-direct-{name}'
                goto(rig, line, back)
                before = rig.shot(root / f'{tag}-0.png')
                rig.im('keyboard-us')
                rig.text(text)
                after = rig.shot(root / f'{tag}-1.png')
                typed = save(rig, note)
                rig.keys('z', mods=('ctrl',))
                undone = save(rig, note)
                boxes = changes(before, after, width)
                if name == 'latin':
                    control[spot] = len(boxes)
                case = dict(spot=spot, im=f'direct-{name}', window=popup(after, width), boxes=boxes,
                            typed_once=typed.count(text) == 1 and typed.replace(text, '', 1) == NOTE,
                            undo_exact=undone == NOTE)
                case['no_candidate_window'] = case['window'] is None
                results.append(case)
                for key in ('typed_once', 'undo_exact', 'no_candidate_window'):
                    if not case[key]:
                        failed.append(f'{tag}: {key}')
                if not (case['typed_once'] and case['undo_exact']):
                    case['texts'] = dict(typed=typed.splitlines()[line], undone=undone.splitlines()[line])
                if undone != NOTE and restore(rig, note) is None:
                    return None, [f'{tag}: the note could not be restored by Undo']
        for spot, line, back in SPOTS:
            for im, word, commit, expected in COMPOSERS:
                tag = f'{spot}-{im}'
                goto(rig, line, back)
                before = rig.shot(root / f'{tag}-0-before.png')
                active = rig.im(im)
                rig.text(word)
                pre = rig.shot(root / f'{tag}-1-preedit.png')
                window = popup(pre, width)
                boxes = changes(before, pre, width, window)
                rig.keys('Escape')
                cancelled = save(rig, note)
                goto(rig, line, back)
                rig.im(im)
                rig.text(word)
                for k in commit:
                    rig.keys(k)
                time.sleep(.8)
                committed = save(rig, note)
                rig.shot(root / f'{tag}-2-committed.png')
                rig.im('keyboard-us')
                time.sleep(.5)
                rig.keys('z', mods=('ctrl',))
                undone = save(rig, note)
                if undone != NOTE and restore(rig, note) is None:
                    return None, [f'{tag}: the note could not be restored by Undo']
                target = NOTE.splitlines()[line]
                landed = committed.splitlines()[line] if len(committed.splitlines()) > line else ''
                case = dict(spot=spot, im=im, active=active, window=window, boxes=boxes, cancelled_exact=cancelled == NOTE,
                            committed_once=committed.count(expected) == 1 and committed.replace(expected, '', 1) == NOTE,
                            in_spot_line=landed.replace(expected, '', 1) == target,
                            undo_exact=undone == NOTE)
                # The preedit row (one change) plus the candidate window just under it, nothing else.
                case['preedit_seen'] = len(boxes) >= 1
                # The edited row also redraws its marker raw (as plain typing does), so
                # the window must open under that row and start inside it.
                row = boxes[-1] if boxes else None
                case['window_under_preedit'] = (window is not None and row is not None
                                                and -4 <= window[1] - row[3] <= 30
                                                and row[0] - 4 <= window[0] <= row[2])
                # #1241: on Wayland, an edit on the quote line under the list scrolls the
                # whole column. Screen checks are void then; the byte checks still hold.
                case['scrolled_1241'] = any(b[3] - b[1] > 300 for b in boxes)
                plain = control.get(spot)
                case['nothing_else_changed'] = len(boxes) == 1 or (plain is not None and len(boxes) <= plain)
                results.append(case)
                screen = () if case['scrolled_1241'] else ('window_under_preedit', 'nothing_else_changed')
                for key in ('preedit_seen', *screen, 'cancelled_exact', 'committed_once', 'in_spot_line', 'undo_exact'):
                    if not case[key]:
                        failed.append(f'{tag}: {key}')
                if not (case['committed_once'] and case['undo_exact'] and case['cancelled_exact']):
                    case['texts'] = dict(cancelled=cancelled.splitlines()[line], committed=committed.splitlines()[line],
                                         undone=undone.splitlines()[line])
                if active != im:
                    failed.append(f'{tag}: input method {im} not active ({active})')
        # Positive control: the candidate window follows the caret down the note.
        tops = {r['spot']: r['window'][1] for r in results if r['im'] == 'pinyin' and r['window']}
        if not ('list' in tops and 'after-bold' in tops and tops['after-bold'] > tops['list'] + 10):
            failed.append(f'control: candidate window does not follow the caret {tops}')
        return results, failed
    finally:
        if proc:
            proc.terminate()
            proc.wait(timeout=10)
        rig.close()


failed_any = invalid = False
# One compositor and input-method session per process: a second one in the
# same process never receives keys. Run each configuration in its own process.
only = os.environ.get('IME_ONLY')  # e.g. "dark-700"
if not only:
    codes = [subprocess.run([sys.executable, __file__, *sys.argv[1:]], env=dict(os.environ, IME_ONLY=f'{t}-{w}')).returncode
             for t in ('light', 'dark') for w in (1366, 700)]
    sys.exit(2 if 2 in codes else 1 if 1 in codes else 0)
for theme in (only.split('-')[0],):
    for width in (int(only.split('-')[1]),):
        results, failed = run(theme, width)
        if results is None:
            print(label, theme, width, 'INVALID', failed, flush=True)
            invalid = True
            continue
        for r in results:
            print(label, theme, width, json.dumps(r, ensure_ascii=False), flush=True)
        print(label, theme, width, 'FAILED' if failed else 'PASS', failed, flush=True)
        if any(f.startswith('control') or 'not active' in f or 'preedit_seen' in f for f in failed):
            invalid = True
        failed_any |= bool(failed)
sys.exit(2 if invalid else 1 if failed_any else 0)
