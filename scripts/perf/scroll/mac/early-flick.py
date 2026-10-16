#!/usr/bin/env python3
"""A flick soon after opening a note, on a Mac (Hedva) (#1312).

    python3 early-flick.py BINARY OUTDIR VAULT NOTE        e.g. big-24k.md

scroll-trace.py sends its first stream 13 s or more after the window is up. Here every rep is
a fresh Okilum: it opens NOTE, waits DELAY_MS (default 3000) after its window appears, and then
`wheel` (wheel.swift, which itself spends about a second bringing the app to the front) sends
EVENTS pixel events of 40 px every INTERVAL_MS (16), so the first flick of a rep meets a note
nothing has been shaped for, and whatever the app does while idle has had about that long.
OUTDIR/early-<note>/rep<N>/ is the layout scroll/analyze.py reads; summary.txt says how long
after the window the stream really started. Needs the QA lock; the app runs under nice -n 10.
"""
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

binary, out, vault, note = sys.argv[1], Path(sys.argv[2]), sys.argv[3], sys.argv[4]
REPS = int(os.environ.get("REPS", "5"))
EVENTS = os.environ.get("WHEEL_EVENTS", "120")
INTERVAL_MS = os.environ.get("WHEEL_INTERVAL_MS", "16")
DELAY_MS = int(os.environ.get("DELAY_MS", "3000"))
HOME = Path.home()
OKQA = os.environ.get("OKQA", str(HOME / "qa/okilum/tools/okqa"))
WHEEL = os.environ.get("WHEEL", str(Path(binary).with_name("wheel")))
SPARKLE = os.environ.get("SPARKLE_DIR", str(HOME / "qa/okilum/src/vendor/sparkle"))
if not os.path.exists("/tmp/okilum-hedva-qa.lock"):
    sys.exit("QA lock is not held")
base = out / f"early-{note.rsplit('.', 1)[0]}"
shutil.rmtree(base, ignore_errors=True)
base.mkdir(parents=True)


def run(*args, check=True):
    return subprocess.run(args, capture_output=True, text=True, check=check)


def now_ms():
    return int(time.time() * 1000)


def slice_trace(log, start_ms, end_ms):
    keep = []
    for line in open(log, errors="replace"):
        m = re.search(r" t=(\d+)", line)
        if m and (start_ms - 1000) * 1000 <= int(m.group(1)) <= (end_ms + 1000) * 1000:
            keep.append(line)
    return "".join(keep)


for rep in range(1, REPS + 1):
    rep_dir = base / f"rep{rep}"
    state = rep_dir / "state"
    state.mkdir(parents=True)
    log = rep_dir / "full-trace.log"
    env = dict(os.environ, OKILUM_STATE_DIR=str(state), XDG_CONFIG_HOME=str(rep_dir / "config"),
               XDG_CACHE_HOME=str(rep_dir / "cache"), XDG_DATA_HOME=str(rep_dir / "data"),
               XDG_STATE_HOME=str(state), OKILUM_FRAME_TRACE=str(log), DYLD_FRAMEWORK_PATH=SPARKLE)
    proc = subprocess.Popen(["nice", "-n", "10", binary, "--vault", vault, "--index-dir", str(rep_dir / "index"),
                             "--note", note], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        run(OKQA, "waitwin", str(proc.pid), "120000", check=False)
        appeared = now_ms()
        run(OKQA, "move", str(proc.pid), "190", "25", "1085", "995", check=False)
        time.sleep(max(0.0, (appeared + DELAY_MS - now_ms()) / 1000))
        start = now_ms()
        res = run(WHEEL, str(proc.pid), EVENTS, "40", INTERVAL_MS, check=False).stdout.strip()
        end = now_ms()
        time.sleep(1.5)
        (rep_dir / "wheel-start.txt").write_text(f"{start}\n")
        (rep_dir / "wheel-end.txt").write_text(f"{end}\n")
        (rep_dir / "trace.log").write_text(slice_trace(log, start, end))
        sent = re.search(r"sent=(\d+)", res)
        (rep_dir / "summary.txt").write_text(
            f"rep={rep} valid={bool(sent and sent.group(1) == EVENTS)} wheel_called_ms_after_window={start - appeared} "
            f"probe={res.replace(' ', '_')}\n")
        print(f"{note} rep{rep}: wheel called {start - appeared} ms after the window, {res}", flush=True)
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
        time.sleep(1.0)
