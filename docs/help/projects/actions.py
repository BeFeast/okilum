"""Drive the real published AppImage through the Projects tutorial and record it.

Run by session.sh (DRIVER=actions.py) on a private Xvfb display. Every caption is
logged with its start time; each step ends with a full-screen frame.
"""
import json
import os
import shutil
import signal
import subprocess
import time
from pathlib import Path

work = Path(os.environ['WORK'])
vault = work / 'practice-vault'
project = vault / 'Kitchen remodel'
evidence = work / 'evidence'
evidence.mkdir()
recorder = subprocess.Popen([
    'ffmpeg', '-hide_banner', '-loglevel', 'warning', '-y', '-f', 'x11grab', '-draw_mouse', '1',
    '-framerate', '15', '-video_size', '1440x960', '-i', os.environ['DISPLAY'],
    '-c:v', 'libx264', '-preset', 'ultrafast', '-crf', '23', '-pix_fmt', 'yuv420p',
    str(work / 'raw.mp4')])
time.sleep(2)
started = time.monotonic()
events = []


def step(text, *args, hold=3):
    events.append({'start': round(time.monotonic() - started, 2), 'text': text})
    if args:
        subprocess.run(['xdotool', *map(str, args)], check=True)
    time.sleep(hold)
    subprocess.run(['import', '-window', 'root', str(work / f'take-{len(events):02}.png')], check=True)


def keep(name, source):
    shutil.copyfile(source, evidence / name)


try:
    step('Start in an ordinary vault. A project adds a Board and records beside your notes.', hold=4)
    step('Open Settings.', 'key', 'ctrl+comma')
    step('Choose Features.', 'mousemove', 397, 278, 'click', 1)
    step('Enable Agents to reveal Projects. No T3 connection is needed.', 'mousemove', 1072, 384, 'click', 1)
    step('Close Settings.', 'mousemove', 1078, 236, 'click', 1)
    step('Open the app menu.', 'mousemove', 1388, 22, 'click', 1)
    step('Choose Projects.', 'mousemove', 1209, 84, 'click', 1)
    step('Projects lists the project folders in this vault. None yet.', 'mousemove', 640, 400, hold=3)
    step('Choose New project.', 'mousemove', 352, 902, 'click', 1)
    step('Folder: use an existing folder, Kitchen remodel.', 'key', 'ctrl+a', 'type', '--delay', 70, 'Kitchen remodel')
    step('Name the project.', 'mousemove', 720, 230, 'click', 1, 'key', 'ctrl+a', 'type', '--delay', 70, 'Kitchen remodel')
    step('Keep Knowledge. Leave Create the project in T3 unchecked.', 'mousemove', 506, 385, hold=4)
    step('Choose Create project.', 'mousemove', 888, 466, 'click', 1, hold=4)
    keep('project.toml', project / '.okilum/project.toml')
    step('The folder is now a project. Open it.', 'mousemove', 376, 195, 'click', 1, hold=4)
    step('Board, Decisions, Reports and Handovers are the project tabs. Manager is optional.', 'mousemove', 160, 60, hold=5)
    step('Choose Create board.', 'mousemove', 441, 534, 'click', 1)
    step('Keep the title, then Create board. Repository is optional.', 'mousemove', 153, 512, 'click', 1, hold=4)
    step('Open Local cards.', 'mousemove', 147, 174, 'click', 1)
    step('Choose Add card.', 'mousemove', 441, 169, 'click', 1)
    step('Enter a task. Todo is the default state.', 'type', '--delay', 70, 'Order tile samples')
    step('Choose Save card.', 'mousemove', 56, 386, 'click', 1)
    keep('board-local.toml', project / '.okilum/board-local.toml')
    step('Return to Manage board.', 'mousemove', 783, 102, 'click', 1)
    step('Close Manage board.', 'mousemove', 768, 100, 'click', 1, hold=4)
    step('Expand the queue: the saved card is on the Board.', 'mousemove', 94, 289, 'click', 1, hold=5)
    step('Open Decisions: read-only notes from the decisions folder.', 'mousemove', 98, 60, 'click', 1, hold=4)
    step('Select a decision to read it.', 'mousemove', 60, 140, 'click', 1, hold=5)
    step('Reports reads the reports folder the same way.', 'mousemove', 165, 60, 'click', 1, hold=5)
    step('Return to Board.', 'mousemove', 36, 60, 'click', 1)
    step('Choose Files to return to your notes.', 'mousemove', 41, 23, 'click', 1, hold=4)
    step('Projects in the top bar reopens a project.', 'mousemove', 100, 22, 'click', 1)
    step('Select Kitchen remodel. Its Board kept the card.', 'mousemove', 158, 148, 'click', 1, hold=6)
finally:
    recorder.send_signal(signal.SIGINT)
    recorder.wait(timeout=30)
    (work / 'events.json').write_text(json.dumps(events, indent=2) + '\n')
