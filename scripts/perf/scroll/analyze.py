#!/usr/bin/env python3
"""Where the frame time of a scroll goes (#1290, #1004): one table row per run directory.

    analyze.py RUNDIR...        RUNDIR holds rep*/trace.log written by a frame-trace build
                                (scripts/perf/frame-trace/patch.py) and rep*/wheel-start.txt /
                                wheel-end.txt (Unix ms) from the host's probe.
    analyze.py --md RUNDIR...   markdown instead of the aligned text table

"cost" is what the app itself spends on a frame, the figure the stage-2 criterion is about
(#1290/#1004): draw + present - the wait for a free drawable (macOS `next_drawable`, which is the
display's, not ours). Windows' Present(0) does not block, so there present counts whole. The
criterion is cost p95 <= 8 ms on the 24k-line note on VM 151 and Linux, <= 16.7 ms on Hedva.

Only frames that redrew (draws >= 1) inside the wheel burst, plus 0.3 s after it, count: gpui
keeps presenting an unchanged scene for about a second afterwards (draws = 0), and a blinking
caret redraws every 500 ms; neither is scroll cost. Times are milliseconds, medians of those
frames; "interval" is the wall-clock time between consecutive redraws, so interval - draw is the
time the main thread did not spend drawing (waiting for the next frame to be due, or a blocked
Present). The [win] lines (Windows) split present into setup (clear + buffer upload), batches
(encoding the primitives) and swap (swap_chain.Present). The [mac] lines (macOS) split it into
drawable (next_drawable, which blocks while every drawable is in flight), encode and commit.
"""
import re
import statistics
import sys
from pathlib import Path

PAIRS = re.compile(r"(\w+)=(-?\d+)")
PHASES = ("draw", "req", "layout", "prepaint", "paint", "post", "present")
# Windows only: where the main thread's time between two frames went, by message class.
MSG_KEYS = ("idle", "wheel", "task", "timer", "move", "other", "paint_msg")
MSG_COUNTS = ("wheel_n", "task_n", "timer_n", "move_n", "other_n", "paint_n", "vs_n", "fg_timeouts", "deferred")


def pct(values, p):
    values = sorted(values)
    if not values:
        return float("nan")
    return values[min(len(values) - 1, int(round(p / 100 * (len(values) - 1))))]


def read_text(path):
    """PowerShell's Tee-Object and Out-File write UTF-16 with a BOM; the trace is UTF-8."""
    raw = path.read_bytes()
    if raw[:2] in (b"\xff\xfe", b"\xfe\xff"):
        return raw.decode("utf-16", errors="replace")
    return raw.decode("utf-8-sig", errors="replace")


def read_rep(rep):
    """Frames of one repetition inside its wheel burst, [win] fields merged into their frame."""
    start = int(read_text(rep / "wheel-start.txt").strip()) * 1000
    end = int(read_text(rep / "wheel-end.txt").strip()) * 1000 + 300_000
    frames, win = [], None
    for line in read_text(rep / "trace.log").splitlines():
        if line.startswith(("[win]", "[mac]")):
            win = {k: int(v) for k, v in PAIRS.findall(line)}
        elif line.startswith("[frame]"):
            frame = {k: int(v) for k, v in PAIRS.findall(line)}
            if win and "swap" in win:
                frame.update({k: v for k, v in win.items() if k != "t"})
                frame["batches"] = max(0, frame["present"] - frame["setup"] - frame["swap"])
            elif win and "drawable" in win:
                frame["drawable"], frame["encode"], frame["commit"] = win["drawable"], win["encode"], win["commit"]
            win = None
            if start <= frame["t"] <= end and frame["draws"] >= 1:
                frames.append(frame)
    return frames


def external(rep):
    """The host's own count (DXGI Present_Start on Windows), written beside the trace."""
    path = rep / "summary.txt"
    return dict(re.findall(r"(\w+)=(\S+)", read_text(path))) if path.exists() else {}


def summarise(run):
    reps = sorted(p for p in run.glob("rep*") if (p / "trace.log").exists())
    frames, intervals, fps, ext = [], [], [], []
    for rep in reps:
        got = read_rep(rep)
        frames += got
        gaps = [(b["t"] - a["t"]) / 1000 for a, b in zip(got, got[1:])]
        intervals += gaps
        if gaps:
            fps.append(1000 * len(gaps) / sum(gaps))
        ext.append(external(rep))
    if not frames:
        return None
    costs = [(f["draw"] + f["present"] - f.get("drawable", 0)) / 1000 for f in frames]

    def med(key):
        values = [f[key] / 1000 for f in frames if key in f]
        return statistics.median(values) if values else float("nan")

    return {
        "run": run.name,
        "reps": f"{len(reps)}",
        "valid": "/".join(str(e.get("valid", "?"))[0] for e in ext),
        "quiet": "/".join(str(e.get("quiet", "?"))[0] for e in ext),
        "capped": "/".join(str(e.get("host_capped", "?"))[0] for e in ext),
        "input_hz": statistics.mean(float(e["wheel_hz"]) for e in ext if "wheel_hz" in e) if any("wheel_hz" in e for e in ext) else float("nan"),
        "redraws": len(frames) / len(reps),
        "fps": statistics.mean(fps) if fps else float("nan"),
        "int50": pct(intervals, 50), "int95": pct(intervals, 95),
        "cost50": pct(costs, 50), "cost95": pct(costs, 95), "costmax": max(costs),
        **{key: med(key) for key in PHASES + ("setup", "batches", "swap", "drawable", "encode", "commit", "cache", "shape")},
        **{key: med(key) for key in MSG_KEYS},
        **{key: statistics.median(f.get(key, 0) for f in frames) for key in MSG_COUNTS},
        "vs_max": statistics.median(f["vs_max"] for f in frames if "vs_max" in f) / 1000 if any("vs_max" in f for f in frames) else float("nan"),
        "vs_mean": statistics.median(f["vs_us"] / f["vs_n"] for f in frames if f.get("vs_n")) / 1000 if any(f.get("vs_n") for f in frames) else float("nan"),
        # An entry frame is one in which the text shaper ran: new text came into view.
        "entry": 100 * sum(f["shapes"] > 0 for f in frames) / len(frames),
        "cache_calls": statistics.median(f["cache_calls"] for f in frames),
        "shapes": statistics.median(f["shapes"] for f in frames),
        "quads": statistics.median(f["quads"] for f in frames),
        "glyphs": statistics.median(f["glyphs"] for f in frames),
        "ext_fps": statistics.mean(float(e["fps"]) for e in ext if "fps" in e) if any("fps" in e for e in ext) else float("nan"),
    }


COLUMNS = (
    ("run", "run", "{}"), ("reps", "reps", "{}"), ("valid", "valid", "{}"), ("quiet", "quiet", "{}"),
    ("capped", "host capped", "{}"), ("input_hz", "wheel Hz", "{:.1f}"), ("redraws", "redraws/rep", "{:.0f}"), ("fps", "fps", "{:.1f}"), ("ext_fps", "ext fps", "{:.1f}"),
    ("int50", "interval p50", "{:.1f}"), ("int95", "p95", "{:.1f}"),
    ("cost50", "frame cost p50", "{:.1f}"), ("cost95", "cost p95", "{:.1f}"), ("costmax", "max", "{:.1f}"),
    ("entry", "entry frames %", "{:.0f}"),
    ("draw", "draw", "{:.1f}"), ("req", "req", "{:.1f}"), ("layout", "layout", "{:.1f}"),
    ("prepaint", "prepaint", "{:.1f}"), ("paint", "paint", "{:.1f}"), ("post", "post", "{:.1f}"),
    ("present", "present", "{:.1f}"), ("setup", "setup", "{:.1f}"), ("batches", "batches", "{:.1f}"),
    ("swap", "swap", "{:.1f}"), ("drawable", "drawable", "{:.1f}"), ("encode", "encode", "{:.1f}"),
    ("commit", "commit", "{:.1f}"), ("cache", "cache", "{:.1f}"), ("shape", "shape", "{:.1f}"),
    ("cache_calls", "cache calls", "{:.0f}"), ("shapes", "shapes", "{:.0f}"),
    ("quads", "quads", "{:.0f}"), ("glyphs", "glyphs", "{:.0f}"),
)


MSG_COLUMNS = (
    ("run", "run", "{}"), ("int50", "interval", "{:.1f}"), ("idle", "idle (GetMessage)", "{:.1f}"),
    ("wheel", "wheel", "{:.1f}"), ("wheel_n", "n", "{:.0f}"), ("task", "fg tasks", "{:.1f}"), ("task_n", "n", "{:.0f}"),
    ("move", "mouse move", "{:.1f}"), ("move_n", "n", "{:.0f}"), ("timer", "timer", "{:.1f}"), ("timer_n", "n", "{:.0f}"),
    ("other", "other msgs", "{:.1f}"), ("other_n", "n", "{:.0f}"), ("paint_msg", "WM_PAINT (draw+present)", "{:.1f}"),
    ("accounted", "accounted", "{:.1f}"), ("unaccounted", "unaccounted", "{:.1f}"),
    ("paint_n", "WM_PAINT n", "{:.0f}"), ("fg_timeouts", "fg 10ms timeouts", "{:.0f}"), ("deferred", "deferred draws", "{:.0f}"),
    ("vs_n", "vsync ticks", "{:.0f}"), ("vs_mean", "vsync wait avg", "{:.1f}"), ("vs_max", "vsync wait max", "{:.1f}"),
)


def accounting(row):
    """interval = idle + messages; what is left over is time in no window procedure and not blocked."""
    parts = [row[k] for k in MSG_KEYS]
    if any(p != p for p in parts):  # nan: not a Windows trace
        return None
    row["accounted"] = sum(parts)
    row["unaccounted"] = row["int50"] - row["accounted"]
    return row


def table(rows, columns, markdown):
    cells = [[fmt.format(r[key]) for key, _, fmt in columns] for r in rows]
    heads = [head for _, head, _ in columns]
    if markdown:
        print("| " + " | ".join(heads) + " |")
        print("|" + "---|" * len(heads))
        for row in cells:
            print("| " + " | ".join(row) + " |")
        return
    widths = [max(len(h), *(len(c[i]) for c in cells)) for i, h in enumerate(heads)] if cells else []
    print("  ".join(h.ljust(w) for h, w in zip(heads, widths)))
    for row in cells:
        print("  ".join(c.ljust(w) for c, w in zip(row, widths)))


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    if not args:
        sys.exit(__doc__)
    rows = [summarise(Path(a)) for a in args]
    for arg, row in zip(args, rows):
        if row is None:
            print(f"# {arg}: no redrawn frames in the wheel burst (no trace.log, or the note did not scroll)")
    rows = [r for r in rows if r]
    markdown = "--md" in sys.argv
    table(rows, COLUMNS, markdown)
    accounted = [r for r in (accounting(dict(r)) for r in rows) if r]
    if accounted:
        print()
        table(accounted, MSG_COLUMNS, markdown)


if __name__ == "__main__":
    main()
