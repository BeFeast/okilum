#!/usr/bin/env python3
"""Large-note Reader benchmark (#653).

Generates a deterministic stress vault (10,000-line note with 2,000 headings,
500 links, big tables and 200 image references), opens it in the release
Reader with the opt-in timing probe, and checks open and scroll frame budgets.

    cargo build --release --locked -p tessera-shell
    python3 scripts/benchmarks/large-note.py [--runs 3] [--json out.json]
        [--baseline earlier.json] [--keep DIR]

Without a DISPLAY it starts Xvfb (Linux). All Reader state, caches and
diagnostics go to a temporary HOME; no user vault or state is read or written.
Timings are host-specific: compare a baseline only with a run from the same
machine in the same session (AGENTS.md).
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import statistics
import struct
import subprocess
import sys
import tempfile
import time
import zlib

REPO = Path(__file__).resolve().parents[2]
LINES = 10_000
HEADINGS = 2_000
LINKS = 500
IMAGE_REFS = 200
IMAGE_FILES = 20
TARGETS = 100
TABLES = 4
TABLE_ROWS = 250
TABLE_COLUMNS = 8

# Budgets for the median of --runs. Frame time is Reader CPU time from render
# to the end of paint; presentation is excluded. Calibrated on a 4-vCPU Linux
# container under Xvfb with Mesa lavapipe (software Vulkan), where the fixed
# Reader measured about 0.67 s open, 26 ms first frame and 8 ms scroll p95,
# and the unfixed one 1.8 s, 720 ms and 119 ms (docs/large-note-performance.md).
# Tighten, never loosen, without review.
BUDGETS = {
    "open_ms": 1200.0,
    "first_frame_ms": 100.0,
    "scroll.p95_ms": 25.0,
    "scroll.max_ms": 50.0,
    "jump.p95_ms": 25.0,
}


def png(width, height, seed):
    rows = bytearray()
    for y in range(height):
        rows.append(0)
        for x in range(width):
            rows += bytes(((x * 3 + seed * 40) % 256, (y * 5 + seed * 17) % 256, (seed * 61) % 256))
    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)
    header = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", zlib.compress(bytes(rows), 9)) + chunk(b"IEND", b"")


def generate(vault):
    """Write the stress vault; return the large note's path and its counts."""
    (vault / "attachments").mkdir(parents=True)
    (vault / "Targets").mkdir()
    for i in range(IMAGE_FILES):
        (vault / "attachments" / f"figure-{i:02}.png").write_bytes(png(480, 270, i))
    for i in range(TARGETS):
        (vault / "Targets" / f"Target {i:03}.md").write_text(f"# Target {i:03}\n\n## Part\n\nBody of target {i}.\n")

    lines = ["---", "title: Large stress note", "tags: [benchmark]", "---", ""]
    links = images = 0
    table_after = {HEADINGS * (t + 1) // (TABLES + 1) for t in range(TABLES)}
    image_every = HEADINGS // IMAGE_REFS
    link_every = HEADINGS // LINKS
    for h in range(HEADINGS):
        level = 1 + (h % 4) if h else 1
        lines += [f"{'#' * level} Section {h:04} heading with *emphasis*", ""]
        text = f"Paragraph {h} with **bold**, `code`, and Unicode текст that wraps across the reading column."
        if h % link_every == 0:
            target = links % TARGETS
            if links % 10 == 9:
                text += f" See [[Missing note {links}]]."
            elif links % 10 == 8:
                text += f" See [[Target {target:03}#Part]]."
            elif links % 10 == 7:
                text += f" See [target {target}](Targets/Target%20{target:03}.md)."
            else:
                text += f" See [[Target {target:03}]]."
            links += 1
        lines += [text, ""]
        if h % image_every == image_every - 1:
            figure = images % IMAGE_FILES
            lines += [f"![[figure-{figure:02}.png]]" if images % 2 else f"![Figure {images}](attachments/figure-{figure:02}.png)", ""]
            images += 1
        if h in table_after:
            lines.append("| " + " | ".join(f"Column {c}" for c in range(TABLE_COLUMNS)) + " |")
            lines.append("|" + "---|" * TABLE_COLUMNS)
            for r in range(TABLE_ROWS):
                lines.append("| " + " | ".join(f"r{r}c{c} value" for c in range(TABLE_COLUMNS)) + " |")
            lines.append("")
    filler = 0
    while len(lines) < LINES:
        lines.append(f"Trailing line {filler} keeps the note at exactly {LINES} lines." if filler % 2 == 0 else "")
        filler += 1
    assert len(lines) == LINES, len(lines)
    assert links == LINKS and images == IMAGE_REFS
    note = vault / "Large.md"
    note.write_text("\n".join(lines) + "\n")
    headings = sum(1 for line in lines if line.startswith("#"))
    assert headings == HEADINGS, headings
    return note, {"lines": len(lines), "headings": headings, "links": links,
                  "image_refs": images, "tables": TABLES, "table_rows": TABLE_ROWS,
                  "bytes": note.stat().st_size}


def start_display():
    if os.environ.get("DISPLAY") or os.environ.get("WAYLAND_DISPLAY"):
        return None, os.environ.get("DISPLAY")
    if not shutil.which("Xvfb"):
        raise SystemExit("No DISPLAY and no Xvfb; install xvfb or run in a desktop session.")
    display = ":653"
    xvfb = subprocess.Popen(["Xvfb", display, "-screen", "0", "1600x1000x24", "-nolisten", "tcp"],
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(1.0)
    if xvfb.poll() is not None:
        raise SystemExit("Xvfb failed to start")
    return xvfb, display


def run_once(binary, note, display, work):
    home = Path(tempfile.mkdtemp(prefix="home-", dir=work))
    report = home / "probe.json"
    env = {k: v for k, v in os.environ.items() if not k.startswith(("TESSERA_", "XDG_"))}
    env.update(HOME=str(home), XDG_STATE_HOME=str(home / "state"), XDG_CACHE_HOME=str(home / "cache"),
               XDG_CONFIG_HOME=str(home / "config"), XDG_DATA_HOME=str(home / "data"),
               TESSERA_READER_PERF_PROBE=str(report))
    if display:
        env["DISPLAY"] = display
    started = time.monotonic()
    result = subprocess.run([str(binary), str(note)], env=env, timeout=300,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    wall = time.monotonic() - started
    if not report.exists():
        raise SystemExit(f"probe wrote no report (exit {result.returncode}):\n{result.stdout[-4000:]}")
    data = json.loads(report.read_text())
    data["process_wall_s"] = wall
    return data


def check_controls(data, counts):
    """Positive controls: the run painted the whole note and actually moved."""
    problems = []
    if data["blocks"] < counts["headings"]:
        problems.append(f"only {data['blocks']} blocks painted; the note has {counts['headings']} headings")
    if data["outline_rows"] != counts["headings"]:
        problems.append(f"outline has {data['outline_rows']} rows, expected {counts['headings']}")
    if data["scroll_end_item"] <= data["scroll_start_item"]:
        problems.append("scripted scrolling did not move the document")
    for target, top in data["jump_items"]:
        if abs(top - target) > 2:
            problems.append(f"jump to block {target} landed on {top}")
    if data["scroll"]["frames"] == 0 or data["jump"]["frames"] == 0:
        problems.append("no timed frames")
    return problems


def metric(data, key):
    value = data
    for part in key.split("."):
        value = value[part]
    return value


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--binary", type=Path, default=REPO / "target/release/tessera")
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--json", type=Path, help="write the median report here")
    parser.add_argument("--baseline", type=Path, help="earlier --json output to compare against")
    parser.add_argument("--keep", type=Path, help="also write the stress vault here")
    parser.add_argument("--no-budget", action="store_true", help="report only; do not fail on budgets")
    args = parser.parse_args()
    if not args.binary.exists():
        raise SystemExit(f"{args.binary} is missing; build it with cargo build --release --locked -p tessera-shell")

    with tempfile.TemporaryDirectory(prefix="tessera-large-note-") as work:
        work = Path(work)
        note, counts = generate(work / "vault")
        if args.keep:
            shutil.copytree(work / "vault", args.keep, dirs_exist_ok=True)
        xvfb, display = start_display()
        try:
            runs = []
            for i in range(args.runs):
                data = run_once(args.binary, note, display, work)
                problems = check_controls(data, counts)
                if problems:
                    raise SystemExit("probe controls failed:\n  " + "\n  ".join(problems))
                print(f"run {i + 1}: open {data['open_ms']:.0f} ms, first frame {data['first_frame_ms']:.1f} ms, "
                      f"scroll p95 {data['scroll']['p95_ms']:.2f} ms, jump p95 {data['jump']['p95_ms']:.2f} ms",
                      file=sys.stderr)
                runs.append(data)
        finally:
            if xvfb:
                xvfb.terminate()
                xvfb.wait()

    keys = ["open_ms", "first_frame_ms", "scroll.p50_ms", "scroll.p95_ms", "scroll.max_ms",
            "jump.p50_ms", "jump.p95_ms", "jump.max_ms"]
    median = {key: statistics.median(metric(run, key) for run in runs) for key in keys}
    summary = {"fixture": counts, "runs": len(runs), "blocks": runs[0]["blocks"], "median": median,
               "host": os.uname().nodename, "budgets": BUDGETS}
    baseline = json.loads(args.baseline.read_text())["median"] if args.baseline else None
    print(f"fixture: {counts}")
    print(f"{'metric':<16}{'median':>12}{'baseline':>12}{'budget':>10}")
    failed = []
    for key in keys:
        budget = BUDGETS.get(key)
        before = f"{baseline[key]:.2f}" if baseline and key in baseline else "-"
        mark = ""
        if budget is not None and median[key] > budget:
            failed.append(key)
            mark = "  OVER"
        print(f"{key:<16}{median[key]:>12.2f}{before:>12}{(f'{budget:.0f}' if budget else '-'):>10}{mark}")
    if args.json:
        args.json.write_text(json.dumps(summary, indent=2) + "\n")
    if failed and not args.no_budget:
        raise SystemExit(f"over budget: {', '.join(failed)}")


if __name__ == "__main__":
    main()
