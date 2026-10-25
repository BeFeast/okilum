#!/usr/bin/env python3
"""Same-job main/PR input comparison on shipped, unpatched release binaries.

Defaults 15% AND 3ms are configurable in ab-policy.json. No absolute CI ceiling.
One complete paired remeasurement follows a valid regression; no-evidence stops.
"""
import argparse
import json
import os
from pathlib import Path
import platform
import statistics
import math
import subprocess
import sys
import time

import budget
import gate
import probe

HERE = Path(__file__).resolve().parent
SCENARIOS = tuple((name, False) for name in budget.FIXTURES) + (("tiny.md", True), ("raw-tail-60k.md", True))
BOUNDARY = "xvfb-software-injection-to-post-frame"


def policy(path):
    value = json.loads(path.read_text())
    budget._keys(value, ("schema", "relative", "minimum_delta_ms", "warmups", "repetitions", "max_attempts", "total_seconds"), "A/B policy")
    if type(value["schema"]) is not int or value["schema"] != 1:
        raise ValueError("unsupported A/B schema")
    for key in ("relative", "minimum_delta_ms"):
        budget._number(value[key], key)
    for key in ("warmups", "repetitions", "max_attempts", "total_seconds"):
        if type(value[key]) is not int or value[key] < 1:
            raise ValueError(f"{key} must be a positive integer")
    if value["max_attempts"] > 2:
        raise ValueError("at most one paired remeasurement is allowed")
    return value


def hardware():
    cpu = Path("/proc/cpuinfo").read_text()
    models = sorted({line.split(":", 1)[1].strip() for line in cpu.splitlines() if line.startswith("model name")})
    if not models:
        raise ValueError("CPU model unavailable")
    return {"cpu_models": models, "logical_cpus": os.cpu_count(),
            "available_cpus": len(os.sched_getaffinity(0)), "machine": platform.machine(),
            "kernel": platform.release(), "runner": os.environ.get("RUNNER_NAME"),
            "image_os": os.environ.get("ImageOS"), "image_version": os.environ.get("ImageVersion"),
            "github_run_id": os.environ.get("GITHUB_RUN_ID"), "job": os.environ.get("GITHUB_JOB")}


def identity(binary):
    binary = binary.resolve(strict=True)
    commit = gate.source_commit(binary)
    if not commit:
        raise ValueError("both binaries require exact-source BUILD.txt receipts")
    receipt = (binary.parent / "BUILD.txt").read_text()
    if "shipped-unpatched" not in receipt:
        raise ValueError("A/B accepts only shipped-unpatched binary receipts")
    return {"path": str(binary), "source_commit": commit, "sha256": gate.sha256(binary), "kind": "shipped-unpatched"}


def summarize(samples, count):
    if len(samples) != count or not all(type(x) in (float, int) and 0 <= x < float("inf") for x in samples):
        raise ValueError("missing or invalid paired input samples")
    return {"median_ms": statistics.median(samples), "p90_ms": sorted(samples)[math.ceil(.9 * count) - 1], "samples_ms": samples}


def compare(main, candidate, config):
    failures = []
    if main.keys() != candidate.keys() or len(main) != len(SCENARIOS):
        raise ValueError("A/B scenario mismatch")
    for name in sorted(main):
        base, observed = main[name]["median_ms"], candidate[name]["median_ms"]
        delta = observed - base
        if delta > base * config["relative"] and delta > config["minimum_delta_ms"]:
            failures.append({"scenario": name, "main_ms": base, "pr_ms": observed, "delta_ms": delta,
                             "relative_delta": delta / base if base else None})
    return failures


def paired(main, candidate, out, config, deadline):
    samples = {role: {} for role in ("main", "pr")}
    evidence, order = [], []
    for name, live in SCENARIOS:
        scenario = f"{'live' if live else 'source'}:{name}"
        for role in samples:
            samples[role][scenario] = []
        for rep in range(config["warmups"] + config["repetitions"]):
            # Counterbalance temporal/cache drift; both roles have identical counts.
            roles = ("main", "pr") if rep % 2 == 0 else ("pr", "main")
            for role in roles:
                if time.monotonic() >= deadline - gate.CLEANUP_SECONDS:
                    raise gate.NoEvidence("paired input deadline exhausted")
                base = out / scenario.replace(":", "-") / f"rep-{rep}-{role}"
                sample, proof = probe.run_once(main if role == "main" else candidate, base,
                                               name, live, rep, deadline, input_only=True)
                order.append({"scenario": scenario, "rep": rep, "role": role})
                evidence.append({"role": role, **proof})
                if rep >= config["warmups"]:
                    samples[role][scenario].append(sample["input_frame_ms"])
    metrics = {role: {name: summarize(values, config["repetitions"]) for name, values in rows.items()}
               for role, rows in samples.items()}
    return {"metrics": metrics, "failures": compare(metrics["main"], metrics["pr"], config),
            "evidence": evidence, "order": order}


def markdown(report):
    lines = ["", "## Same-runner input A/B", "", f"**{report['result']}**", "",
             "Shipped, unpatched release binaries. Xvfb injection → matching post-frame; not native display latency.",
             "No absolute CI ceiling. Product 33ms Linux / 16ms M4 acceptance belongs on real hardware.",
             f"Defaults (configurable): {report.get('policy')}", f"CPU/runner: {report.get('hardware')}"]
    for role, row in report.get("binaries", {}).items():
        lines.append(f"{role}: source `{row['source_commit']}`, binary SHA256 `{row['sha256']}`.")
    if report.get("error"):
        lines.append(f"NO EVIDENCE: {gate._cell(report['error'])}")
    for at in report["attempts"]:
        lines.extend(["", f"### Paired attempt {at['number']}: {at['result']}", "",
                      "| Scenario | Main median ms | PR median ms | Delta ms | Delta % |", "|---|---:|---:|---:|---:|"])
        for name, m in at.get("metrics", {}).get("main", {}).items():
            p = at["metrics"]["pr"][name]["median_ms"]; b = m["median_ms"]
            ratio = f"{100 * (p-b)/b:.2f}" if b else "n/a"
            lines.append(f"| {name} | {b:.3f} | {p:.3f} | {p-b:.3f} | {ratio} |")
    lines.append("Unsupported large Live Preview: " + ", ".join(budget.UNSUPPORTED) + ". Scroll is excluded from this gate.")
    return "\n".join(lines) + "\n"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("main_binary", type=Path)
    parser.add_argument("pr_binary", type=Path)
    parser.add_argument("outdir", type=Path)
    parser.add_argument("--policy", type=Path, default=HERE / "ab-policy.json")
    parser.add_argument("--summary", type=Path, default=os.environ.get("GITHUB_STEP_SUMMARY") or None)
    args = parser.parse_args(argv)
    report = {"schema": 1, "boundary": BOUNDARY, "result": "NO_EVIDENCE", "exit_code": 2,
              "attempts": [], "binaries": {}, "error": None}
    created = False
    started = time.monotonic()
    try:
        config = policy(args.policy)
        report["policy"] = config
        report["hardware"] = hardware()
        deadline = started + config["total_seconds"]
        main_binary, pr_binary = args.main_binary.resolve(strict=True), args.pr_binary.resolve(strict=True)
        report["binaries"] = {"main": identity(main_binary), "pr": identity(pr_binary)}
        if args.summary and args.summary.resolve() in {
            main_binary, pr_binary, args.policy.resolve(),
            main_binary.parent / "BUILD.txt", pr_binary.parent / "BUILD.txt",
            args.outdir.resolve() / "result.json",
        }:
            raise ValueError("summary must not overwrite input/provenance/result")
        args.outdir.mkdir(parents=True, exist_ok=False)
        created = True
        for number in range(1, config["max_attempts"] + 1):
            for role, binary in (("main", main_binary), ("pr", pr_binary)):
                gate._check_binary(binary, report["binaries"][role]["sha256"])
            at = {"number": number, "result": "NO_EVIDENCE"}; report["attempts"].append(at)
            try:
                at.update(paired(main_binary, pr_binary, args.outdir / f"attempt-{number}", config, deadline))
            finally:
                for role, binary in (("main", main_binary), ("pr", pr_binary)):
                    gate._check_binary(binary, report["binaries"][role]["sha256"])
            at["result"] = "FAIL" if at["failures"] else "PASS"
            report["result"], report["exit_code"] = at["result"], int(bool(at["failures"]))
            if not at["failures"]:
                break
    except (OSError, ValueError, subprocess.SubprocessError, gate.NoEvidence, probe.NoEvidence) as error:
        report["result"], report["exit_code"], report["error"] = "NO_EVIDENCE", 2, str(error)
    report["duration_seconds"] = time.monotonic() - started
    if created:
        (args.outdir / "result.json").write_text(json.dumps(report, indent=2) + "\n")
    if args.summary:
        with args.summary.open("a") as stream:
            stream.write(markdown(report))
    print(f"{report['result']}: same-runner input A/B")
    return report["exit_code"]


if __name__ == "__main__":
    sys.exit(main())
