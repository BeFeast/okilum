#!/usr/bin/env python3
"""The Linux CI guard of the scroll frame cost (#1290): fail when a frame grows with the note.

    budget.py HEAD_RUN BIG_RUN

HEAD_RUN and BIG_RUN are the probe's run directories (scroll/linux/scroll-trace.py) for
big-head-320.md and big-24k.md: the same blocks on screen, 320 against 24,000 lines. A frame
that costs more on the long note does work per frame that grows with the document, which is
what #1294 removed (every frame rebuilt a set over all of the note's links: request_layout
went from 0.9 to 3.9 ms on the 24k note).

Only `draw` counts here: the app's own work for a frame (render, layout, prepaint, paint).
Under Xvfb, present is the software rasteriser and says nothing about a real display.

Checks (milliseconds, medians and p95 over the redrawn frames of all reps):
  growth   req p50 (big) - req p50 (head)  <= REQ_GROWTH_MS   (default 1.0)
           draw p50 (big) - draw p50 (head) <= DRAW_GROWTH_MS  (default 3.0)
  budget   draw p95 (big)                   <= DRAW_P95_MS     (default 30.0)
The growth checks are the guard: they compare two notes in one run on one machine, so the
runner's speed cancels out. Calibrated on CT141 (two runs each): main +0.07 / +0.13 ms req and
+0.94 / +0.79 ms draw; with #1294 reverted +4.13 / +2.97 ms req. The absolute budget only
catches a frame that is slow on every note; p95 alone reached 21 ms on an unchanged build there,
so it has wide headroom.
Exit 0: all hold; 1: a check failed; 2: the run proves nothing (a rep did not scroll, or too
few redrawn frames), which is a failure too.
"""
import importlib.util
import os
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
MIN_REDRAWS = 20  # per rep: fewer means the stream did not exercise scrolling


def load_analyze():
    spec = importlib.util.spec_from_file_location("analyze", HERE / "analyze.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def limits():
    return {key: float(os.environ.get(key, default)) for key, default in
            (("REQ_GROWTH_MS", 1.0), ("DRAW_GROWTH_MS", 3.0), ("DRAW_P95_MS", 30.0))}


def measure(analyze, run):
    """Medians/p95 of draw and req over every rep, and why the run proves nothing (if it does not)."""
    reps = sorted(p for p in Path(run).glob("rep*") if (p / "trace.log").exists())
    problems, draws, reqs = [], [], []
    if not reps:
        problems.append(f"{run}: no reps")
    for rep in reps:
        summary = analyze.external(rep)
        if summary.get("valid") != "True":
            problems.append(f"{rep}: the note did not scroll (window pixels unchanged)")
        frames = analyze.read_rep(rep)
        if len(frames) < MIN_REDRAWS:
            problems.append(f"{rep}: {len(frames)} redrawn frames in the burst (< {MIN_REDRAWS})")
        draws += [f["draw"] / 1000 for f in frames]
        reqs += [f["req"] / 1000 for f in frames]
    if not draws:
        return None, problems
    return {
        "frames": len(draws),
        "draw50": analyze.pct(draws, 50),
        "draw95": analyze.pct(draws, 95),
        "req50": analyze.pct(reqs, 50),
    }, problems


def verdict(head, big, limit):
    """The failed checks, as lines; empty when every check holds."""
    failed = []
    req_growth = big["req50"] - head["req50"]
    draw_growth = big["draw50"] - head["draw50"]
    if req_growth > limit["REQ_GROWTH_MS"]:
        failed.append(f"request_layout grows with the note: {big['req50']:.2f} ms on 24k lines against "
                      f"{head['req50']:.2f} ms on 320 (+{req_growth:.2f} > {limit['REQ_GROWTH_MS']:.2f})")
    if draw_growth > limit["DRAW_GROWTH_MS"]:
        failed.append(f"draw grows with the note: {big['draw50']:.2f} ms on 24k lines against "
                      f"{head['draw50']:.2f} ms on 320 (+{draw_growth:.2f} > {limit['DRAW_GROWTH_MS']:.2f})")
    if big["draw95"] > limit["DRAW_P95_MS"]:
        failed.append(f"draw p95 on the 24k note is {big['draw95']:.2f} ms (> {limit['DRAW_P95_MS']:.2f})")
    return failed


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    analyze = load_analyze()
    limit = limits()
    (head, head_problems), (big, big_problems) = measure(analyze, sys.argv[1]), measure(analyze, sys.argv[2])
    for name, row in (("320 lines", head), ("24k lines", big)):
        if row:
            print(f"{name:10} frames {row['frames']:4}  draw p50 {row['draw50']:6.2f}  p95 {row['draw95']:6.2f}  "
                  f"req p50 {row['req50']:6.2f} ms")
    problems = head_problems + big_problems
    if problems or not (head and big):
        print("NO EVIDENCE: " + "; ".join(problems or ["no redrawn frames"]))
        return 2
    failed = verdict(head, big, limit)
    for line in failed:
        print("FAILED: " + line)
    if not failed:
        print(f"PASS: the frame does not grow with the note (req +{big['req50'] - head['req50']:.2f} ms, "
              f"draw +{big['draw50'] - head['draw50']:.2f} ms) and draw p95 {big['draw95']:.2f} ms "
              f"<= {limit['DRAW_P95_MS']:.2f}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
