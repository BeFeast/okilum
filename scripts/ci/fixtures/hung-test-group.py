#!/usr/bin/env python3
"""Synthetic process tree: one hung test and daemonized, TERM-resistant helpers."""
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

work = Path(sys.argv[1])
role = sys.argv[2]
if role != 'test':
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
(work / f'{role}.pid').write_text(str(os.getpid()))
if role == 'test':
    subprocess.Popen([sys.executable, __file__, str(work), 'daemon'], start_new_session=True)
elif role == 'daemon':
    # Double fork: the intermediate parent exits and the grandchild changes
    # session again, modelling dbus/Syncthing helpers escaping a process group.
    if os.fork():
        raise SystemExit(0)
    os.setsid()
    (work / 'detached.pid').write_text(str(os.getpid()))
    subprocess.Popen([sys.executable, __file__, str(work), 'helper'])
    (work / 'ready').touch()
while True:
    time.sleep(1)
