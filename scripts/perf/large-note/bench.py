#!/usr/bin/env python3
"""Large-note Reader benchmark with budgets (#653).

    bench.py BINARY [--runs 3] [--json OUT] [--no-budget] [--keep DIR]
             [--note large|code] [--theme KEY] [--appearance light|dark]

Writes the stress vault of fixture.py (10,000 lines, 2,000 headings, 500 links,
four 250-row tables, 200 image references), opens Large.md in a release Reader
with the opt-in probe (OKILUM_READER_PERF_PROBE, src/reader_perf_probe.rs) and
checks the median of --runs against BUDGETS. Each run has its own HOME, state,
index and vault copy; no user state is read or written. Without a DISPLAY it
starts Xvfb.

Frame times are Reader CPU time from `Reader::render` to the end of the body's
paint; presentation (software rasterisation under Xvfb) is excluded. Open is
the launch clock at the first painted frame that holds the document's blocks.
A run whose positive controls fail (document painted, outline complete,
scrolling moved, every outline jump landed on its heading) is rejected, never
counted. Timings are host-specific: compare only runs from one machine in one
session (AGENTS.md).

`--note code` opens Code.md instead: 10,000 lines of fenced Rust, Python,
JavaScript and JSON under 435 headings (#3156). `--theme` and `--appearance`
start every launch with that theme and mode; the probe reports what it drew
with and a run on another theme or mode is rejected. Theme A/B: the same
binary and note, alternating themes, one session.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import statistics
import subprocess
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parent))
import fixture  # noqa: E402

# Budgets for the median of --runs, on a 4-6 vCPU Linux host under Xvfb.
# `open_over_control_ms` is what the large note adds to the open of the small
# control note in the same launch setup: the part of open time this note owns.
# Tighten, never loosen, without review.
BUDGETS = {
    "open_ms": 3000.0,
    "open_over_control_ms": 300.0,
    "document_stage_ms": 400.0,
    "first_frame_ms": 100.0,
    "scroll.p95_ms": 25.0,
    "scroll.max_ms": 50.0,
    "jump.p95_ms": 25.0,
    "jump.max_ms": 60.0,
}
KEYS = ["open_ms", "control_open_ms", "open_over_control_ms", "document_stage_ms",
        "control_document_stage_ms", "first_frame_ms", "scroll.p50_ms", "scroll.p95_ms", "scroll.max_ms",
        "jump.p50_ms", "jump.p95_ms", "jump.max_ms",
        # Text layout work per phase, counted by the TextView and its inline
        # elements (#3154): extra layout passes show here, not in frame counts.
        "layout.scroll.text_view_layouts", "layout.scroll.inline_layouts", "layout.scroll.inline_prepaints",
        "layout.jump.text_view_layouts", "layout.jump.inline_layouts", "layout.jump.inline_prepaints"]
PHASES = {
    "note_open_core_preparation": "worker_ms",
    "note_open_rendered": "since_open_ms",
    "note_open_accept": "duration_ms",
    "note_open_parse": "duration_ms",
    "note_open_prepare": "duration_ms",
    "note_open_set_text": "duration_ms",
    "note_open_heading_inventory": "duration_ms",
    "note_open_parsed": "since_open_ms",
    "note_open_body_painted": "layout_frame_ms",
}


class NoEvidence(RuntimeError):
    pass


def start_display():
    if os.environ.get("DISPLAY"):
        return None, os.environ["DISPLAY"]
    if not shutil.which("Xvfb"):
        raise SystemExit("No DISPLAY and no Xvfb")
    read, write = os.pipe()
    xvfb = subprocess.Popen(["Xvfb", "-displayfd", str(write), "-screen", "0", "1600x1000x24",
                             "-nolisten", "tcp"], pass_fds=(write,),
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    os.close(write)
    with os.fdopen(read) as stream:
        number = stream.readline().strip()
    if not number or xvfb.poll() is not None:
        raise SystemExit("Xvfb failed to start")
    return xvfb, f":{number}"


def phases(log):
    """The note_open_* diagnostics of one launch, and the launch clock (ms)
    at which every logged phase was first reported: where the open time went."""
    found, timeline = {}, {}
    if log.exists():
        for line in log.read_text(errors="replace").splitlines():
            try:
                row = json.loads(line)
            except ValueError:
                continue
            if "phase" in row and "elapsed_ms" in row:
                timeline.setdefault(row["phase"], round(row["elapsed_ms"], 1))
            key = PHASES.get(row.get("phase"))
            if key and key in row.get("details", {}):
                found.setdefault(row["phase"], row["details"][key])
    return found, timeline


def document_stage(timeline):
    """From the loader handing the prepared note to the UI to the first frame
    with its blocks: the TextView parse and publication of this note, without
    the launch, the vault scan or the software display's start-up."""
    try:
        return timeline["document_first_paint"] - timeline["first_event_received"]
    except KeyError as missing:
        raise NoEvidence(f"diagnostic {missing} not logged") from None


def ui_state(theme=None, appearance=None):
    """reader-ui.json for a launch that starts in this theme and mode."""
    state = {"version": 1}
    if theme:
        state["theme"] = theme
    if appearance:
        state["appearance"] = appearance
    return state


def run_once(binary, vault_source, display, work, rep, note=fixture.NOTE, theme=None, appearance=None):
    base = work / f"run-{rep}-{Path(note).stem}"
    home, vault = base / "home", base / "vault"
    shutil.copytree(vault_source, vault)
    state = home / "okilum"
    state.mkdir(parents=True)
    if theme or appearance:
        (state / "reader-ui.json").write_text(json.dumps(ui_state(theme, appearance)))
    runtime = home / "runtime"
    runtime.mkdir(mode=0o700)
    report = base / "probe.json"
    env = {k: v for k, v in os.environ.items() if not k.startswith(("OKILUM_", "XDG_"))}
    env.update(HOME=str(home), WAYLAND_DISPLAY="", DISPLAY=display,
               XDG_CONFIG_HOME=str(home / "config"), XDG_CACHE_HOME=str(home / "cache"),
               XDG_DATA_HOME=str(home / "data"), XDG_STATE_HOME=str(home / "state"),
               XDG_RUNTIME_DIR=str(runtime),
               OKILUM_STATE_DIR=str(state), OKILUM_READER_PERF_PROBE=str(report))
    with (base / "app.log").open("wb") as log:
        try:
            result = subprocess.run([str(binary), "--vault", str(vault), "--index-dir", str(base / "index"),
                                     "--note", note], env=env, timeout=180,
                                    stdout=log, stderr=subprocess.STDOUT)
        except subprocess.TimeoutExpired:
            raise NoEvidence(f"run {rep}: the probe did not finish in 180 s (see {base / 'app.log'})")
    if not report.exists():
        raise NoEvidence(f"run {rep}: no probe report (exit {result.returncode}, see {base / 'app.log'})")
    data = json.loads(report.read_text())
    data["phases"], data["timeline"] = phases(state / "reader-diagnostic.log")
    if (vault / note).read_bytes() != (vault_source / note).read_bytes():
        raise NoEvidence(f"run {rep}: the note changed on disk")
    return data


def controls(data, counts, theme=None, appearance=None):
    """Positive controls: the run painted the whole note, moved and jumped,
    in the theme and mode asked for."""
    problems = []
    drawn = data.get("theme") or {}
    if theme and drawn.get("key") != theme:
        problems.append(f"drawn with theme {drawn.get('key')!r}, asked for {theme!r}")
    if appearance and drawn.get("dark") != (appearance == "dark"):
        problems.append(f"drawn with dark={drawn.get('dark')!r}, asked for {appearance}")
    if data["blocks"] < counts["headings"]:
        problems.append(f"only {data['blocks']} blocks; the note has {counts['headings']} headings")
    if data["outline_rows"] != counts["headings"]:
        problems.append(f"outline has {data['outline_rows']} rows, expected {counts['headings']}")
    if data["scroll_end_item"] <= data["scroll_start_item"]:
        problems.append("scripted scrolling did not move the document")
    if len(data["jumps"]) != 4:
        problems.append(f"{len(data['jumps'])} outline jumps recorded, expected 4")
    for jump in data["jumps"]:
        if jump["top_block"] != jump["target_block"]:
            problems.append(f"outline row {jump['outline_row']} → block {jump['target_block']} "
                            f"landed on {jump['top_block']}")
    if data["scroll"]["frames"] == 0 or data["jump"]["frames"] == 0:
        problems.append("no timed frames")
    layout = data.get("layout") or {}
    if any((layout.get(phase) or {}).get("inline_layouts", 0) == 0 for phase in ("scroll", "jump")):
        problems.append("the layout counters did not count any inline layout")
    return problems


def metric(data, key):
    for part in key.split("."):
        data = data[part]
    return data


def summarize(runs, counts, baseline=None):
    median = {key: statistics.median(metric(run, key) for run in runs) for key in KEYS}
    over = [key for key, budget in BUDGETS.items() if median[key] > budget]
    lines = [f"{'metric':<16}{'median':>10}{'runs':>28}{'budget':>9}"]
    for key in KEYS:
        budget = BUDGETS.get(key)
        each = " / ".join(f"{metric(run, key):.1f}" for run in runs)
        lines.append(f"{key:<16}{median[key]:>10.1f}{each:>28}{(f'{budget:.0f}' if budget else '-'):>9}"
                     + ("  OVER" if key in over else ""))
    return {"fixture": counts, "runs": runs, "median": median, "budgets": BUDGETS, "over": over,
            "host": os.uname().nodename, "cpus": os.cpu_count()}, "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--json", type=Path)
    parser.add_argument("--no-budget", action="store_true", help="report only; do not fail on budgets")
    parser.add_argument("--keep", type=Path, help="keep every run's state and logs here")
    parser.add_argument("--note", choices=["large", "code"], default="large")
    parser.add_argument("--theme", help="theme key, e.g. graphite or obsidianite")
    parser.add_argument("--appearance", choices=["light", "dark"])
    args = parser.parse_args()
    look = {"theme": args.theme, "appearance": args.appearance}
    if not args.binary.is_file():
        raise SystemExit(f"{args.binary} is missing")
    work = Path(tempfile.mkdtemp(prefix="okilum-large-note-"))
    try:
        vault_source = work / "vault"
        _, counts = fixture.write(vault_source, check=True)
        note = fixture.NOTE
        if args.note == "code":
            note, counts = fixture.CODE_NOTE, fixture.code_note()[1]
        xvfb, display = start_display()
        try:
            runs = []
            for rep in range(1, args.runs + 1):
                data = run_once(args.binary.resolve(), vault_source, display, work, rep, note, **look)
                control = run_once(args.binary.resolve(), vault_source, display, work, rep, fixture.SMALL, **look)
                if control["blocks"] == 0 or control["outline_rows"] != 5:
                    raise NoEvidence(f"run {rep}: the control note did not open: {json.dumps(control)}")
                data["control_open_ms"] = control["open_ms"]
                data["open_over_control_ms"] = data["open_ms"] - control["open_ms"]
                data["control_timeline"] = control["timeline"]
                data["document_stage_ms"] = document_stage(data["timeline"])
                data["control_document_stage_ms"] = document_stage(control["timeline"])
                problems = controls(data, counts, **look)
                if problems:
                    raise NoEvidence(f"run {rep} controls failed:\n  " + "\n  ".join(problems)
                                     + f"\n  report: {json.dumps(data)}")
                print(f"run {rep}: open {data['open_ms']:.0f} ms (control {control['open_ms']:.0f} ms), first frame {data['first_frame_ms']:.1f} ms, "
                      f"scroll p95 {data['scroll']['p95_ms']:.1f} ms, jump p95 {data['jump']['p95_ms']:.1f} ms, "
                      f"phases {json.dumps(data['phases'])}\n  timeline {json.dumps(data['timeline'])}"
                      f"\n  control timeline {json.dumps(control['timeline'])}", file=sys.stderr)
                runs.append(data)
        finally:
            if xvfb:
                xvfb.terminate()
                xvfb.wait()
    except NoEvidence as error:
        print(f"NO_EVIDENCE: {error}", file=sys.stderr)
        if args.keep:
            shutil.copytree(work, args.keep, dirs_exist_ok=True)
        shutil.rmtree(work, ignore_errors=True)
        return 2
    if args.keep:
        shutil.copytree(work, args.keep, dirs_exist_ok=True)
    shutil.rmtree(work, ignore_errors=True)
    summary, table = summarize(runs, counts)
    summary.update(note=note, **look)
    landed = [jump["landed_frame"] for run in runs for jump in run["jumps"]]
    table += f"\noutline jumps landed in frame (0 = first after the click): {landed}"
    print(f"fixture: {counts}\n{table}")
    if args.json:
        args.json.write_text(json.dumps(summary, indent=2) + "\n")
    if summary["over"] and not args.no_budget:
        print(f"OVER BUDGET: {', '.join(summary['over'])}", file=sys.stderr)
        return 1
    print("PASS" if not summary["over"] else "REPORT-ONLY: over budget")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
