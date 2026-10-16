#!/usr/bin/env python3
"""What an opened note costs while nobody uses it, on Linux under X11 (#1312).

    python3 idle-frames.py BINARY OUTDIR VAULT NOTE [FLICK]

A fresh Okilum opens NOTE and is left alone. The frame trace gives the frames it draws, second
by second from the moment its window appeared (an app that warms something up draws a burst and
then nothing), and the process's CPU time (utime + stime from /proc, not `ps %cpu`) and resident
size are read over the last TAIL_S seconds (default 30) of a WATCH_S (default 45) long run.
With FLICK set, the note is scrolled once, 5 s after the window appears, by NOTCHES notches
(120), and what follows is watched the same way. The trace must show frames at all (a build
without the trace gives a void run). Prints one JSON object and writes it to OUTDIR/idle.json.
"""
import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

binary, out, vault, note = sys.argv[1], Path(sys.argv[2]).resolve(), os.path.abspath(sys.argv[3]), sys.argv[4]
FLICK = len(sys.argv) > 5 and sys.argv[5] == "FLICK"
WATCH_S = float(os.environ.get("WATCH_S", "45"))
TAIL_S = float(os.environ.get("TAIL_S", "30"))
NOTCHES = int(os.environ.get("NOTCHES", "120"))
shutil.rmtree(out, ignore_errors=True)
out.mkdir(parents=True)
CLK = os.sysconf("SC_CLK_TCK")


def run(*args, env=None, check=True):
    return subprocess.run(args, env=env, capture_output=True, text=True, check=check)


def cpu_seconds(pid):
    fields = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
    return (int(fields[11]) + int(fields[12])) / CLK  # utime, stime


def rss_mb(pid):
    for line in open(f"/proc/{pid}/status"):
        if line.startswith("VmRSS:"):
            return int(line.split()[1]) / 1024
    return 0.0


home = out / "home"
home.mkdir()
log = out / "trace.log"
env = dict(os.environ, WAYLAND_DISPLAY="", HOME=str(home), XDG_CONFIG_HOME=str(home / "config"),
           XDG_CACHE_HOME=str(home / "cache"), XDG_DATA_HOME=str(home / "data"),
           XDG_STATE_HOME=str(home / "state"), OKILUM_STATE_DIR=str(home / "okilum"),
           OKILUM_FRAME_TRACE=str(log))
proc = subprocess.Popen([binary, "--vault", vault, "--index-dir", str(out / "index"), "--note", note],
                        env=env, stdout=subprocess.DEVNULL, stderr=open(out / "app.log", "w"))
try:
    win = None
    for _ in range(400):
        time.sleep(0.05)
        found = run("xdotool", "search", "--onlyvisible", "--pid", str(proc.pid), env=env, check=False).stdout.split()
        if found:
            win = found[-1]
            break
    if not win:
        sys.exit("no window")
    appeared = time.time()
    run("xdotool", "windowmove", win, "0", "0", "windowsize", win, "1366", "768", "windowfocus", win,
        "mousemove", "1300", "700", env=env)
    flicked_at = None
    samples = []
    while time.time() - appeared < WATCH_S:
        elapsed = time.time() - appeared
        if FLICK and flicked_at is None and elapsed >= 5:
            run("xdotool", "mousemove", "600", "400", env=env)
            flicked_at = time.time()
            run("xdotool", "click", "--repeat", str(NOTCHES), "--delay", "16", "5", env=env)
            flicked_end = time.time()
        samples.append((time.time(), cpu_seconds(proc.pid), rss_mb(proc.pid)))
        time.sleep(0.5)
    frames = []
    for line in open(log, errors="replace"):
        m = re.search(r" t=(\d+)", line)
        if m and line.startswith("[frame]"):
            frames.append(int(m.group(1)) / 1e6)
    per_second = {}
    for t in frames:
        second = int(t - appeared)
        if 0 <= second < WATCH_S:
            per_second[second] = per_second.get(second, 0) + 1
    last_frame = max((t - appeared for t in frames), default=None)
    tail_from = appeared + WATCH_S - TAIL_S
    tail = [s for s in samples if s[0] >= tail_from]
    cpu = 100 * (tail[-1][1] - tail[0][1]) / (tail[-1][0] - tail[0][0])
    frames_in_tail = sum(1 for t in frames if t >= tail_from)
    result = {
        "note": note, "flick": FLICK, "watch_s": WATCH_S, "tail_s": TAIL_S,
        "frames_total": len(frames),
        "frames_per_second_from_window": [per_second.get(s, 0) for s in range(int(WATCH_S))],
        "last_frame_s_after_window": round(last_frame, 2) if last_frame is not None else None,
        "flick_started_s_after_window": round(flicked_at - appeared, 2) if flicked_at else None,
        "flick_ended_s_after_window": round(flicked_end - appeared, 2) if flicked_at else None,
        "frames_in_tail": frames_in_tail,
        "cpu_percent_tail": round(cpu, 3),
        "rss_mb_first": round(samples[0][2], 1), "rss_mb_last": round(samples[-1][2], 1),
    }
    (out / "idle.json").write_text(json.dumps(result, indent=1) + "\n")
    print(json.dumps(result))
finally:
    proc.terminate()
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        proc.kill()
    shutil.rmtree(home, ignore_errors=True)
