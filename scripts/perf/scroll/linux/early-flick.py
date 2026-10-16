#!/usr/bin/env python3
"""A flick soon after opening a note, on Linux under X11 (#1312).

    python3 early-flick.py BINARY OUTDIR VAULT NOTE        e.g. big-24k.md

scroll-trace.py starts its first stream seven to nineteen seconds after the window is up (the
vault is indexed, the note laid out, a 3 s idle check), which is longer than a person waits.
Here every rep is a fresh Okilum: it opens NOTE, waits DELAY_MS (default 400) after its window
appears, and sends NOTCHES wheel notches, one every 16 ms, so the first flick of a rep meets a
note nothing has been shaped for, and whatever the app does while idle has had only DELAY_MS.
BINARY is a frame-trace build (scripts/perf/build-linux-trace.sh); DISPLAY must name the X
server. OUTDIR/early-<note>/rep<N>/ is the layout scroll/analyze.py reads, with a summary
that says whether the note really scrolled.
"""
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

binary, out, vault, note = sys.argv[1], Path(sys.argv[2]).resolve(), os.path.abspath(sys.argv[3]), sys.argv[4]
REPS = int(os.environ.get("REPS", "5"))
NOTCHES = int(os.environ.get("NOTCHES", "120"))
DELAY_MS = int(os.environ.get("DELAY_MS", "400"))
base = out / f"early-{note.rsplit('.', 1)[0]}"
shutil.rmtree(base, ignore_errors=True)
base.mkdir(parents=True)


def run(*args, env=None, check=True):
    return subprocess.run(args, env=env, capture_output=True, text=True, check=check)


def now_ms():
    return int(time.time() * 1000)


def slice_trace(log, start_ms, end_ms):
    keep = []
    for line in open(log, errors="replace"):
        m = re.search(r" t=(\d+)", line)
        if m and (start_ms - 1000) * 1000 <= int(m.group(1)) <= (end_ms + 1000) * 1000:
            keep.append(line)
    return "".join(keep)


def window_hash(win, env):
    for command in (["xwd", "-silent", "-id", win], ["import", "-window", win, "rgb:-"]):
        try:
            shot = subprocess.run(command, env=env, capture_output=True, check=False).stdout
        except FileNotFoundError:
            continue
        if shot:
            return hashlib.sha256(shot).hexdigest()
    return ""


summary = []
for rep in range(1, REPS + 1):
    rep_dir = base / f"rep{rep}"
    home = rep_dir / "home"
    home.mkdir(parents=True)
    log = rep_dir / "full-trace.log"
    env = dict(os.environ, WAYLAND_DISPLAY="", HOME=str(home), XDG_CONFIG_HOME=str(home / "config"),
               XDG_CACHE_HOME=str(home / "cache"), XDG_DATA_HOME=str(home / "data"),
               XDG_STATE_HOME=str(home / "state"), OKILUM_STATE_DIR=str(home / "okilum"),
               OKILUM_FRAME_TRACE=str(log))
    proc = subprocess.Popen([binary, "--vault", vault, "--index-dir", str(rep_dir / "index"), "--note", note],
                            env=env, stdout=subprocess.DEVNULL, stderr=open(rep_dir / "app.log", "w"))
    try:
        win = None
        for _ in range(240):
            time.sleep(0.05)
            found = run("xdotool", "search", "--onlyvisible", "--pid", str(proc.pid), env=env, check=False).stdout.split()
            if found:
                win = found[-1]
                break
        if not win:
            sys.exit(f"rep {rep}: no window (see {rep_dir / 'app.log'})")
        appeared = now_ms()
        run("xdotool", "windowmove", win, "0", "0", "windowsize", win, "1366", "768", "windowfocus", win,
            "mousemove", "600", "400", env=env)
        time.sleep(max(0.0, (appeared + DELAY_MS - now_ms()) / 1000))
        before = window_hash(win, env)
        start = now_ms()
        run("xdotool", "click", "--repeat", str(NOTCHES), "--delay", "16", "5", env=env)
        end = now_ms()
        time.sleep(1.5)
        after = window_hash(win, env)
        (rep_dir / "wheel-start.txt").write_text(f"{start}\n")
        (rep_dir / "wheel-end.txt").write_text(f"{end}\n")
        (rep_dir / "trace.log").write_text(slice_trace(log, start, end))
        scrolled = bool(before and after and before != after)
        (rep_dir / "summary.txt").write_text(
            f"rep={rep} valid={scrolled} delay_ms={start - appeared} wheel_ms={end - start}\n")
        summary.append({"rep": rep, "valid": scrolled, "delay_ms": start - appeared, "wheel_ms": end - start})
        print(f"{note} rep{rep}: scrolled {scrolled}, flick {start - appeared} ms after the window, wheel {end - start} ms",
              flush=True)
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
        shutil.rmtree(home, ignore_errors=True)
json.dump(summary, open(base / "summary.json", "w"), indent=1)
