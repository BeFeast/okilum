#!/usr/bin/env python3
"""Scroll probe on a Mac (Hedva: Intel, macOS 12, headless virtual display at 30 Hz), #1004.

    python3 scroll-trace.py BINARY OUTDIR VAULT [mode:note ...]   e.g. reader:Long-10k.md

BINARY is a frame-trace build (scripts/perf/frame-trace/patch.py); VAULT holds the notes of
gen-notes.py; mode is reader, live or source. Per scenario Okilum starts once with
OKILUM_FRAME_TRACE=<file>, and REPS (3) wheel streams of 120 pixel events of 40 px, one every
16 ms (the stream of the #1004 measurements; WHEEL_EVENTS / WHEEL_INTERVAL_MS change it; SHOTS=1 takes frame shots instead), are posted by `wheel` (wheel.swift; it does not
capture the screen, a capture loads WindowServer and perturbs the timing). OUTDIR/<scenario>/
rep<N>/ gets the slice of the trace of that stream, wheel-start.txt / wheel-end.txt (Unix ms)
for scripts/perf/scroll/analyze.py, and before/after screenshots next to the scenario's idle
control (frames in 3 s without input: 0 in Reader, a few in the editor modes for the caret).
Hedva is shared: this needs the QA lock (/tmp/okilum-hedva-qa.lock, taken with shlock by the
session), and the app runs under nice -n 10. `okqa` is the Mac QA kit's window tool.
"""
import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

binary, out, vault = sys.argv[1], Path(sys.argv[2]), sys.argv[3]
scenarios = sys.argv[4:] or ["reader:Long-LP-700.md", "reader:Long-10k.md", "source:Long-10k.md"]
REPS = int(os.environ.get("REPS", "3"))
# The measured stream: WHEEL_EVENTS notches of 40 px, one every WHEEL_INTERVAL_MS. The default is the
# 60 Hz stream of #1004; 130 ms is one notch per frame on Hedva's 7.5 fps virtual display, i.e. the
# same distance per frame as the default has on a 60 Hz display.
EVENTS = int(os.environ.get("WHEEL_EVENTS", "120"))
INTERVAL_MS = os.environ.get("WHEEL_INTERVAL_MS", "16")
# SHOTS=1: instead of measured reps, one stream per scenario with window shots inside it (at 60 % and 85 %
# of it), 300 ms after its end and 2 s after it, for looking at the frames (a shot loads WindowServer, so
# these runs are never timed).
SHOTS = os.environ.get("SHOTS") == "1"
HOME = Path.home()
OKQA = os.environ.get("OKQA", str(HOME / "qa/okilum/tools/okqa"))
# build-mac-trace.sh puts the compiled injector next to the binary
WHEEL = os.environ.get("WHEEL", str(Path(binary).with_name("wheel")))
SPARKLE = os.environ.get("SPARKLE_DIR", str(HOME / "qa/okilum/src/vendor/sparkle"))
if not os.path.exists("/tmp/okilum-hedva-qa.lock"):
    sys.exit("QA lock is not held")
shutil.rmtree(out, ignore_errors=True)
out.mkdir(parents=True)


def run(*args, check=True):
    return subprocess.run(args, capture_output=True, text=True, check=check)


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


summary = []
for scenario in scenarios:
    mode, note = scenario.split(":")[:2]
    label = f"{mode}-{note.split('.')[0]}"
    base = out / label
    (base / "state").mkdir(parents=True)
    log = base / "trace.log"
    env = dict(os.environ, OKILUM_STATE_DIR=str(base / "state"), XDG_CONFIG_HOME=str(base / "config"),
               XDG_CACHE_HOME=str(base / "cache"), XDG_DATA_HOME=str(base / "data"),
               XDG_STATE_HOME=str(base / "state"), OKILUM_FRAME_TRACE=str(log), DYLD_FRAMEWORK_PATH=SPARKLE)
    proc = subprocess.Popen(["nice", "-n", "10", binary, "--vault", vault, "--index-dir", str(base / "index"),
                             "--note", note], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        print(label, "waitwin:", run(OKQA, "waitwin", str(proc.pid), "120000", check=False).stdout.strip()[:140], flush=True)
        run(OKQA, "move", str(proc.pid), "190", "25", "1085", "995", check=False)
        time.sleep(14 if "10k" in note else 8)  # the vault is indexed and the note laid out
        if mode == "live":
            run(OKQA, "key", str(proc.pid), "cmd+shift+e", "5000", "800", check=False)
        elif mode == "source":
            run(OKQA, "key", str(proc.pid), "cmd+e", "5000", "800", check=False)
        time.sleep(2.0)
        run(OKQA, "shot", str(proc.pid), str(base / "before.png"), check=False)
        a = frames(log)
        time.sleep(3)
        idle = frames(log) - a
        if SHOTS:
            run(WHEEL, str(proc.pid), str(EVENTS), "-40", "16", check=False)  # back to the top
            time.sleep(2.5)
            stream = subprocess.Popen([WHEEL, str(proc.pid), str(EVENTS), "40", INTERVAL_MS],
                                      stdout=subprocess.PIPE, text=True)
            # Two shots inside the stream: on a display that draws every 130 ms the first frames of
            # the stream have not reached the screen at its middle (the shot then shows the start).
            began = time.monotonic()
            for share in (0.6, 0.85):
                time.sleep(max(0.0, began + EVENTS * float(INTERVAL_MS) / 1000 * share - time.monotonic()))
                run(OKQA, "shot", str(proc.pid), str(base / f"in-stream-{int(share * 100)}.png"), check=False)
            res = stream.communicate()[0].strip()
            time.sleep(0.3)
            run(OKQA, "shot", str(proc.pid), str(base / "end-plus-300ms.png"), check=False)
            time.sleep(1.5)
            run(OKQA, "shot", str(proc.pid), str(base / "end-plus-2s.png"), check=False)
            summary.append({"label": label, "shots": True, "probe": res})
            print(f"{label} shots: probe {res}", flush=True)
            continue
        for rep in range(1, REPS + 1):
            run(WHEEL, str(proc.pid), str(EVENTS), "-40", "16", check=False)  # back to the top (not measured)
            time.sleep(2.5)
            start = now_ms()
            res = run(WHEEL, str(proc.pid), str(EVENTS), "40", INTERVAL_MS, check=False).stdout.strip()
            end = now_ms()
            time.sleep(1.5)
            rep_dir = base / f"rep{rep}"
            rep_dir.mkdir()
            (rep_dir / "wheel-start.txt").write_text(f"{start}\n")
            (rep_dir / "wheel-end.txt").write_text(f"{end}\n")
            (rep_dir / "trace.log").write_text(slice_trace(log, start, end))
            sent = re.search(r"sent=(\d+)", res)
            (rep_dir / "summary.txt").write_text(
                f"rep={rep} valid={bool(sent and int(sent.group(1)) == EVENTS)} idle_frames_3s={idle} stream={EVENTS}x40px/{INTERVAL_MS}ms probe={res.replace(' ', '_')}\n")
            summary.append({"label": label, "rep": rep, "idle_frames_3s": idle, "probe": res})
            print(f"{label} rep{rep}: probe {res}, idle {idle}", flush=True)
        run(OKQA, "shot", str(proc.pid), str(base / "after.png"), check=False)
    finally:
        subprocess.run(["osascript", "-e", 'tell application id "com.befeast.okilum" to quit'], capture_output=True)
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
        json.dump(summary, open(out / "summary.json", "w"), indent=1)
