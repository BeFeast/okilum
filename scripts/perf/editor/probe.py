#!/usr/bin/env python3
"""#1639 Linux editor gate, using the existing perf-linux binary/display.

One warmup and five independent launches per scenario. The endpoint is the
diagnostic callback AFTER a frame containing the exact source generation, under
Xvfb software rendering; it does not establish GPU completion or scanout. Scroll
uses the existing trace's CPU draw duration, excluding software rasterization.
All sources, UI state, recovery and diagnostic files belong to this isolated run.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import re
import subprocess
import sys
import time

import fixtures


class NoEvidence(RuntimeError):
    pass


def command(*args, env=None):
    return subprocess.run(args, env=env, check=True, capture_output=True,
                          timeout=10).stdout


def events(path):
    result = []
    if path.exists():
        for line in path.read_text(errors="replace").splitlines():
            try:
                result.append(json.loads(line))
            except ValueError:
                pass  # the writer may currently be appending the last line
    return result


def wait_for(predicate, proc, deadline, description):
    end = min(deadline, time.monotonic() + 12)
    while time.monotonic() < end:
        if proc.poll() is not None:
            raise NoEvidence(f"app exited {proc.returncode} while waiting for {description}")
        value = predicate()
        if value:
            return value
        time.sleep(.01)
    raise NoEvidence(f"deadline waiting for {description}")


def frame_event(log, purpose, after_ns=0):
    return next((row["details"] for row in events(log)
                 if row.get("phase") == "editor_latency_frame"
                 and row["details"].get("purpose") == purpose
                 and row["details"]["monotonic_ns"] > after_ns
                 and row["details"].get("outcome") == "painted"), None)


def window_pixels(win, env):
    data = command("xwd", "-silent", "-id", win, env=env)
    if len(data) < 256:
        raise NoEvidence("empty window capture")
    return hashlib.sha256(data).hexdigest()


def draw_samples(path, start_us, end_us):
    result = []
    for line in path.read_text(errors="replace").splitlines():
        if not line.startswith("[frame]"):
            continue
        fields = dict(re.findall(r"(\w+)=(-?\d+)", line))
        if start_us <= int(fields["t"]) <= end_us and int(fields["draws"]) > 0:
            result.append(int(fields["draw"]) / 1000)
    if len(result) < 5:
        raise NoEvidence(f"only {len(result)} scroll redraws; need at least 5")
    return result


def run_once(binary, base, name, live, rep, deadline):
    home = base / "home"
    state = home / "okilum"
    vault = base / "notes"
    state.mkdir(parents=True)
    vault.mkdir()
    source = fixtures.notes()[name]
    (vault / name).write_bytes(source)
    layout = {"note": name, "source": True, "live_preview": live}
    (state / "reader-ui.json").write_text(json.dumps({
        "version": 1, "appearance": "light", "last_layout": layout,
        "vaults": {str(vault): layout},
    }))
    log = state / "reader-diagnostic.log"
    trace = base / "trace.log"
    env = dict(os.environ, WAYLAND_DISPLAY="", HOME=str(home),
               XDG_CONFIG_HOME=str(home / "config"), XDG_CACHE_HOME=str(home / "cache"),
               XDG_DATA_HOME=str(home / "data"), XDG_STATE_HOME=str(home / "state"),
               OKILUM_STATE_DIR=str(state), OKILUM_FRAME_TRACE=str(trace),
               OKILUM_EDITOR_LATENCY_DIAGNOSTICS="1",
               OKILUM_EDITOR_LAYOUT_DIAGNOSTICS="1")
    launch_ns = time.monotonic_ns()
    app_log = (base / "app.log").open("wb")
    proc = subprocess.Popen([str(binary), "--vault", str(vault), "--index-dir",
                             str(base / "index"), "--note", name], env=env,
                            stdout=app_log, stderr=subprocess.STDOUT)
    try:
        opened = wait_for(lambda: frame_event(log, "open"), proc, deadline, "source content frame")
        sample = {"open_first_ms": (opened["monotonic_ns"] - launch_ns) / 1e6}
        if live:
            full = wait_for(lambda: frame_event(log, "live"), proc, deadline, "full LP frame")
            if not full["live"] or full.get("limited") is not False:
                raise NoEvidence("the small fixture did not produce a complete Live Preview")
            sample["open_full_ms"] = (full["monotonic_ns"] - launch_ns) / 1e6

        def find_window():
            found = subprocess.run(["xdotool", "search", "--onlyvisible", "--pid", str(proc.pid)],
                                   env=env, capture_output=True, timeout=2).stdout.split()
            return found[-1].decode() if len(found) == 1 else None

        win = wait_for(find_window, proc, deadline, "one visible app window")
        command("xdotool", "windowsize", win, "1366", "768", "windowfocus", win, env=env)
        command("xdotool", "key", "ctrl+Home", env=env)
        # Let the resize and focus frame complete before the sparse edit.
        time.sleep(.1)
        before_edit = window_pixels(win, env)
        sent_ns = time.monotonic_ns()
        command("xdotool", "key", "x", env=env)
        edited = wait_for(lambda: frame_event(log, "edit", sent_ns), proc, deadline,
                          "matching mutation/content frame")
        if (edited["document"] != opened["document"]
                or edited["generation"] <= opened["generation"]
                or edited["mutation_ns"] < sent_ns):
            raise NoEvidence("edit frame has a stale or foreign source identity")
        sample["input_frame_ms"] = (edited["monotonic_ns"] - sent_ns) / 1e6
        if window_pixels(win, env) == before_edit:
            raise NoEvidence("the edit frame left window pixels unchanged")
        # Assert the actual editor buffer changed; a caret redraw is insufficient.
        command("xdotool", "key", "ctrl+a", "ctrl+c", env=env)
        def buffer_changed():
            copied = subprocess.run(["xclip", "-selection", "clipboard", "-out"], env=env,
                                    capture_output=True, timeout=2)
            return copied.returncode == 0 and copied.stdout == b"x" + source
        wait_for(buffer_changed, proc, deadline, "injected edit in the actual copied buffer")
        command("xdotool", "key", "Left", "ctrl+Home", env=env)
        command("xdotool", "mousemove", "600", "400", env=env)
        time.sleep(.1)
        before = window_pixels(win, env)
        start_ns = time.monotonic_ns()
        start_us = time.time_ns() // 1000
        command("xdotool", "click", "--repeat", "50", "--delay", "10", "5", env=env)
        end_us = time.time_ns() // 1000
        time.sleep(.05)
        after = window_pixels(win, env)
        scroll = wait_for(lambda: next((row["details"] for row in reversed(events(log))
                                       if row.get("phase") == "editor_layout_sample"
                                       and row["details"]["scroll"][1] < -1), None),
                          proc, deadline, "nonzero editor scroll offset")
        if before == after:
            raise NoEvidence("scroll left window pixels unchanged")
        draws = draw_samples(trace, start_us, end_us)
        # One observation per repetition: p90 CPU draw cost during its scroll.
        sample["scroll_frame_ms"] = sorted(draws)[math.ceil(.9 * len(draws)) - 1]
        proof = {"scenario": f"{'live' if live else 'source'}:{name}", "rep": rep,
                 "opened": opened, "edited": edited, "scroll": scroll,
                 "scroll_start_ns": start_ns, "scroll_draws_ms": draws,
                 "pixels_changed": True, "buffer_verified": True}
        (base / "sample.json").write_text(json.dumps({"sample": sample, "evidence": proof}, indent=2))
        return sample, proof
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=3)
        app_log.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("outdir", type=Path)
    args = parser.parse_args()
    binary, out = args.binary.resolve(), args.outdir.resolve()
    if out.exists():
        raise NoEvidence(f"output already exists: {out}")
    out.mkdir(parents=True)
    if fixtures.manifest() != json.loads((fixtures.HERE / "manifest.json").read_text()):
        raise NoEvidence("fixture manifest drift")
    summary = {"schema": 1, "boundary": "xvfb-software-post-frame", "samples": {},
               "unsupported": [f"live:{name}" for name in fixtures.notes() if name != "tiny.md"],
               "evidence": []}
    deadline = time.monotonic() + 295
    started = time.monotonic()
    try:
        scenarios = [(name, False) for name in fixtures.notes()] + [("tiny.md", True)]
        for name, live in scenarios:
            key = f"{'live' if live else 'source'}:{name}"
            summary["samples"][key] = {}
            for rep in range(6):
                base = out / key.replace(":", "-") / f"rep{rep}"
                sample, proof = run_once(binary, base, name, live, rep, deadline)
                for metric, value in sample.items():
                    summary["samples"][key].setdefault(metric, []).append(value)
                summary["evidence"].append(proof)
                print(f"{key} rep={rep}{' warmup' if rep == 0 else ''} {sample}", flush=True)
    finally:
        (out / "runtime.json").write_text(json.dumps({"duration_seconds": time.monotonic() - started}))
        (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")


if __name__ == "__main__":
    try:
        main()
    except (NoEvidence, subprocess.SubprocessError, OSError, ValueError, KeyError) as error:
        print(f"NO EVIDENCE: {error}", file=sys.stderr)
        sys.exit(2)
