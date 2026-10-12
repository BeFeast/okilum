#!/usr/bin/env python3
"""Scroll probe on Linux under X11 (an Xvfb display in CI, or any X server), #1290.

    python3 scroll-trace.py BINARY OUTDIR VAULT [mode:note ...]   e.g. reader:big-24k.md

BINARY is a frame-trace build (scripts/perf/build-linux-trace.sh); VAULT holds the notes of
gen-notes.py; mode is reader, live or source. DISPLAY must name the X server. Per scenario
Okilum starts once with OKILUM_FRAME_TRACE=<file> in a 1366x768 window, and REPS (3) wheel
streams of 120 notches, one every 16 ms (the stream of the #1004 measurements), are sent with
xdotool. OUTDIR/<scenario>/rep<N>/ gets the slice of the trace of that stream and
wheel-start.txt / wheel-end.txt (Unix ms) for scroll/analyze.py, as the Mac and Windows probes
write them, and summary.txt says whether the rep is valid.

Positive controls: the window's pixels before and after a rep must differ (the note really
scrolled), and the trace must hold redrawn frames in the burst. Negative control: frames in 3 s
without input (0 in Reader, the caret's in the editor modes).
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

binary, out, vault = sys.argv[1], Path(sys.argv[2]).resolve(), os.path.abspath(sys.argv[3])
scenarios = sys.argv[4:] or ["reader:big-head-320.md", "reader:big-24k.md"]
REPS = int(os.environ.get("REPS", "3"))
NOTCHES = int(os.environ.get("NOTCHES", "120"))
shutil.rmtree(out, ignore_errors=True)
out.mkdir(parents=True)


def run(*args, env=None, check=True):
    return subprocess.run(args, env=env, capture_output=True, text=True, check=check)


def frames(log):
    try:
        return sum(1 for line in open(log, errors="replace") if line.startswith("[frame]"))
    except FileNotFoundError:
        return 0


def now_ms():
    return int(time.time() * 1000)


def slice_trace(log, start_ms, end_ms):
    """Lines of the trace written between the two stamps (+- 1 s), so each rep stands alone."""
    keep = []
    for line in open(log, errors="replace"):
        m = re.search(r" t=(\d+)", line)
        if m and (start_ms - 1000) * 1000 <= int(m.group(1)) <= (end_ms + 1000) * 1000:
            keep.append(line)
    return "".join(keep)


def window_hash(win, env):
    """SHA-256 of the window's pixels (xwd), to tell a scroll from an idle window."""
    shot = subprocess.run(["xwd", "-silent", "-id", win], env=env, capture_output=True, check=False).stdout
    return hashlib.sha256(shot).hexdigest() if shot else ""


summary = []
for scenario in scenarios:
    mode, note = scenario.split(":")[:2]
    label = f"{mode}-{note.rsplit('.', 1)[0]}"
    base = out / label
    home = base / "home"
    home.mkdir(parents=True)
    log = base / "trace.log"
    env = dict(os.environ, WAYLAND_DISPLAY="", HOME=str(home), XDG_CONFIG_HOME=str(home / "config"),
               XDG_CACHE_HOME=str(home / "cache"), XDG_DATA_HOME=str(home / "data"),
               XDG_STATE_HOME=str(home / "state"), OKILUM_STATE_DIR=str(home / "okilum"),
               OKILUM_FRAME_TRACE=str(log))
    proc = subprocess.Popen([binary, "--vault", vault, "--index-dir", str(base / "index"), "--note", note],
                            env=env, stdout=subprocess.DEVNULL, stderr=open(base / "app.log", "w"))
    try:
        win = None
        for _ in range(240):
            time.sleep(0.25)
            found = run("xdotool", "search", "--onlyvisible", "--pid", str(proc.pid), env=env, check=False).stdout.split()
            if found:
                win = found[-1]
                break
        if not win:
            sys.exit(f"{label}: no window (see {base / 'app.log'})")
        run("xdotool", "windowmove", win, "0", "0", "windowsize", win, "1366", "768", "windowfocus", win, env=env)
        time.sleep(float(os.environ.get("SETTLE", "14")))  # the vault is indexed and the note laid out
        if mode == "live":
            run("xdotool", "key", "ctrl+shift+e", env=env)
        elif mode == "source":
            run("xdotool", "key", "ctrl+e", env=env)
        run("xdotool", "mousemove", "600", "400", env=env)
        time.sleep(2.0)
        a = frames(log)
        time.sleep(3)
        idle = frames(log) - a
        for rep in range(1, REPS + 1):
            run("xdotool", "key", "ctrl+Home", env=env)  # back to the top (not measured)
            time.sleep(2.5)
            before = window_hash(win, env)
            start = now_ms()
            run("xdotool", "click", "--repeat", str(NOTCHES), "--delay", "16", "5", env=env)
            end = now_ms()
            time.sleep(1.5)
            after = window_hash(win, env)
            rep_dir = base / f"rep{rep}"
            rep_dir.mkdir()
            (rep_dir / "wheel-start.txt").write_text(f"{start}\n")
            (rep_dir / "wheel-end.txt").write_text(f"{end}\n")
            (rep_dir / "trace.log").write_text(slice_trace(log, start, end))
            scrolled = bool(before and after and before != after)
            (rep_dir / "summary.txt").write_text(
                f"rep={rep} valid={scrolled} idle_frames_3s={idle} wheel_ms={end - start}\n")
            summary.append({"label": label, "rep": rep, "valid": scrolled, "idle_frames_3s": idle,
                            "wheel_ms": end - start})
            print(f"{label} rep{rep}: scrolled {scrolled}, wheel {end - start} ms, idle {idle}", flush=True)
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
        json.dump(summary, open(out / "summary.json", "w"), indent=1)
