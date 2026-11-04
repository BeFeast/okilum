#!/usr/bin/env python3
"""Validate native reference controls and summarize GPUI CPU draw cost/latency."""
import argparse
import json
import math
from pathlib import Path


def stats(values):
    values = sorted(values)
    if not values:
        raise ValueError("No samples: absence is not a passing measurement")
    return {
        "frames": len(values),
        "p50_ms": values[math.ceil(0.50 * len(values)) - 1],
        "p95_ms": values[math.ceil(0.95 * len(values)) - 1],
        "over_16_7_share": sum(value > 16.7 for value in values) / len(values),
        "max_ms": values[-1],
    }


def controls(directory, phase, ranges):
    from PIL import Image, ImageChops

    def changed(kind, rep, crop=None):
        before = Image.open(directory / f"{phase}-{kind}-{rep}-before.png").convert("RGB")
        after = Image.open(directory / f"{phase}-{kind}-{rep}-after.png").convert("RGB")
        if before.size != after.size:
            raise ValueError("Capture viewport changed during the measurement")
        if crop:
            before, after = before.crop(crop), after.crop(crop)
        if ImageChops.difference(before, after).getbbox() is None:
            raise ValueError(f"{kind} {rep}: physical input did not change the view")

    for entry in ranges:
        kind, rep = entry["scenario"], entry["rep"]
        if kind == "resize":
            if abs(entry["width_after"] - entry["width_before"]) < 80:
                raise ValueError("Splitter did not change the production panel width")
            # Prove visible geometry changed, excluding the composer caret and
            # the pointer's splitter hover. Persisted width alone is insufficient.
            changed(kind, rep, (400, 46, 800, 120))
        elif kind == "click":
            # First fixture card; exclude unrelated Manager repaint/caret changes.
            changed(kind, rep, (14, 100, 148, 370))
        elif kind == "scroll":
            # Reference Manager history only: exclude composer caret, Board,
            # header status and chip/tooltips from the source movement proof.
            changed(kind, rep, (670, 150, 1180, 600))


def analyze(directory, phase):
    frames, presents = [], []
    for line in (directory / f"{phase}.log").read_text().splitlines():
        if line.startswith("PERF "):
            frames.append(json.loads(line[5:]))
        elif line.startswith("PRESENT "):
            presents.append(json.loads(line[8:]))
    ranges = json.loads((directory / f"{phase}-ranges.json").read_text())
    controls(directory, phase, ranges)
    result = {}
    for scenario in ("scroll", "resize", "click"):
        selected = [entry for entry in ranges if entry["scenario"] == scenario]
        if not selected:
            raise ValueError(f"Missing {scenario} input ranges")
        samples = []
        for entry in selected:
            samples_in_range = [frame for frame in frames
                                if entry["start"] <= frame["unix"] <= entry["end"]]
            if not samples_in_range:
                raise ValueError(f"{scenario} {entry['rep']}: no drawn frame in input burst")
            samples.extend(frame["draw_ms"] for frame in samples_in_range)
        result[scenario] = stats(samples)
        if scenario != "click":
            seconds = sum(entry["end"] - entry["start"] for entry in selected)
            result[scenario]["input_seconds"] = seconds
            result[scenario]["drawn_frames_per_second"] = len(samples) / seconds
        if scenario == "click":
            latencies = []
            for entry in selected:
                present = next((frame for frame in presents
                                if frame["unix"] >= entry["start"]), None)
                if present is None or present["unix"] > entry["end"]:
                    raise ValueError("No presentation following a real card click")
                latencies.append((present["unix"] - entry["start"]) * 1000)
            result[scenario]["send_to_present"] = stats(latencies)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("phase")
    parser.add_argument("--gate", action="store_true", help="require scroll/resize p95 <= 8 ms")
    args = parser.parse_args()
    try:
        result = analyze(args.directory, args.phase)
    except (ValueError, OSError, KeyError) as error:
        parser.exit(2, f"No verdict: {error}\n")
    print(json.dumps(result, indent=2))
    (args.directory / f"{args.phase}-validated.json").write_text(json.dumps(result, indent=2) + "\n")
    if args.gate and any(result[key]["p95_ms"] > 8 for key in ("scroll", "resize")):
        parser.exit(1, "Frame-cost gate failed\n")


if __name__ == "__main__":
    main()
