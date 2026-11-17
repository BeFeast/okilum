#!/usr/bin/env python3
"""Same-job main/PR input comparison on shipped, unpatched release binaries.

A scenario regresses only when all of these hold on one runner (ab-policy.json):
- the PR median is worse than main by more than 15% AND 3 ms, or the PR median
  exceeds the absolute ceiling while main stays within it;
- an exact one-sided rank test says the PR samples are slower than main's
  (family-wise alpha across all scenarios, Bonferroni);
- the paired remeasurement of the same two binaries regresses the same scenario.
Main over the ceiling on this runner is no evidence, not a regression.
"""
import argparse
import itertools
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
POLICY_KEYS = ("schema", "relative", "minimum_delta_ms", "absolute_ceiling_ms", "family_alpha",
               "warmups", "repetitions", "max_attempts", "total_seconds")
# Exact enumeration is C(2n, n) splits; 8 repetitions is 12,870 splits.
MAX_REPETITIONS = 8


def policy(path):
    value = json.loads(path.read_text())
    budget._keys(value, POLICY_KEYS, "A/B policy")
    if type(value["schema"]) is not int or value["schema"] != 2:
        raise ValueError("unsupported A/B schema")
    for key in ("relative", "minimum_delta_ms", "absolute_ceiling_ms", "family_alpha"):
        budget._number(value[key], key)
    for key in ("warmups", "repetitions", "max_attempts", "total_seconds"):
        if type(value[key]) is not int or value[key] < 1:
            raise ValueError(f"{key} must be a positive integer")
    if value["max_attempts"] > 2:
        raise ValueError("at most one paired remeasurement is allowed")
    if value["absolute_ceiling_ms"] <= 0:
        raise ValueError("absolute_ceiling_ms must be positive")
    if not 0 < value["family_alpha"] < 1:
        raise ValueError("family_alpha must be between 0 and 1")
    if value["repetitions"] > MAX_REPETITIONS:
        raise ValueError(f"repetitions above {MAX_REPETITIONS} make the exact rank test too slow")
    # A policy whose rank test can never reject would silently disable the gate.
    if 1 / math.comb(2 * value["repetitions"], value["repetitions"]) > scenario_alpha(value):
        raise ValueError("repetitions too few: the rank test could never reject at family_alpha")
    return value


def scenario_alpha(config):
    return config["family_alpha"] / len(SCENARIOS)


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


def _u(main, candidate):
    return sum((y > x) + 0.5 * (y == x) for x in main for y in candidate)


def rank_test(main, candidate):
    """Exact one-sided Mann-Whitney test that the candidate samples are larger.

    Returns (U, p): U counts main/PR pairs where the PR sample is slower (ties
    count half); p is the share of all equal-size relabelings of the pooled
    samples whose U is at least as large. No distribution is assumed, which
    matters because hosted samples are bimodal (frame-phase quantization).
    """
    pooled, size = list(main) + list(candidate), len(candidate)
    observed, hits, total = _u(main, candidate), 0, 0
    for chosen in itertools.combinations(range(len(pooled)), size):
        picked = set(chosen)
        relabeled = _u([pooled[i] for i in range(len(pooled)) if i not in picked], [pooled[i] for i in chosen])
        hits += relabeled >= observed - 1e-9
        total += 1
    return observed, hits / total


def compare(main, candidate, config):
    """Per-scenario verdicts for one paired attempt."""
    if main.keys() != candidate.keys() or len(main) != len(SCENARIOS):
        raise ValueError("A/B scenario mismatch")
    alpha, ceiling = scenario_alpha(config), config["absolute_ceiling_ms"]
    rows = []
    for name in sorted(main):
        base, observed = main[name]["median_ms"], candidate[name]["median_ms"]
        delta = observed - base
        u, p = rank_test(main[name]["samples_ms"], candidate[name]["samples_ms"])
        reasons = []
        if delta > base * config["relative"] and delta > config["minimum_delta_ms"]:
            reasons.append("relative")
        if observed > ceiling:
            reasons.append("ceiling")
        if base > ceiling:
            verdict = "main-over-ceiling"
        elif reasons and p <= alpha:
            verdict = "regression"
        elif reasons:
            verdict = "not-significant"
        else:
            verdict = "pass"
        rows.append({"scenario": name, "main_ms": base, "pr_ms": observed, "delta_ms": delta,
                     "relative_delta": delta / base if base else None, "rank_u": u,
                     "rank_pairs": len(main[name]["samples_ms"]) * len(candidate[name]["samples_ms"]),
                     "rank_p": p, "alpha": alpha, "reasons": reasons, "verdict": verdict})
    return rows


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
    scenarios = compare(metrics["main"], metrics["pr"], config)
    return {"metrics": metrics, "scenarios": scenarios,
            "failures": [row for row in scenarios if row["verdict"] == "regression"],
            "evidence": evidence, "order": order}


def markdown(report):
    config = report.get("policy") or {}
    lines = ["", "## Same-runner input A/B", "", f"**{report['result']}** ({report.get('mode', 'strict')})", "",
             "Shipped, unpatched release binaries. Xvfb injection → matching post-frame; not native display latency."]
    if config:
        lines.append(f"A scenario fails only when the PR median is worse than main by >{100 * config['relative']:g}% "
                     f"and >{config['minimum_delta_ms']:g} ms (or exceeds the {config['absolute_ceiling_ms']:g} ms "
                     f"absolute ceiling while main does not), the exact rank test gives p ≤ "
                     f"{config['family_alpha']:g}/{len(SCENARIOS)}, and the paired remeasurement "
                     "regresses the same scenario. Product 33ms Linux / 16ms M4 acceptance belongs on real hardware.")
    lines += [f"Policy: {config}", f"CPU/runner: {report.get('hardware')}"]
    for role, row in report.get("binaries", {}).items():
        lines.append(f"{role}: source `{row['source_commit']}`, binary SHA256 `{row['sha256']}`.")
    if report.get("confirmed_failures"):
        lines.append("Confirmed in every attempt: " + ", ".join(report["confirmed_failures"]))
    if report.get("error"):
        lines.append(f"NO EVIDENCE: {gate._cell(report['error'])}")
    for at in report["attempts"]:
        lines.extend(["", f"### Paired attempt {at['number']}: {at['result']}", "",
                      "| Scenario | Main median ms | PR median ms | Delta ms | Delta % | PR slower pairs | Rank p | Verdict |",
                      "|---|---:|---:|---:|---:|---:|---:|---|"])
        for row in at.get("scenarios", []):
            ratio = f"{100 * row['relative_delta']:.2f}" if row["relative_delta"] is not None else "n/a"
            lines.append(f"| {row['scenario']} | {row['main_ms']:.3f} | {row['pr_ms']:.3f} | {row['delta_ms']:.3f} "
                         f"| {ratio} | {row['rank_u']:g}/{row['rank_pairs']} | {row['rank_p']:.4f} | {row['verdict']} |")
    lines.append("Unsupported large Live Preview: " + ", ".join(budget.UNSUPPORTED) + ". Scroll is excluded from this gate.")
    return "\n".join(lines) + "\n"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("main_binary", type=Path)
    parser.add_argument("pr_binary", type=Path)
    parser.add_argument("outdir", type=Path)
    parser.add_argument("--policy", type=Path, default=HERE / "ab-policy.json")
    parser.add_argument("--summary", type=Path, default=os.environ.get("GITHUB_STEP_SUMMARY") or None)
    parser.add_argument("--report-only", action="store_true",
                        help="preserve performance FAIL in reports without failing the job; evidence errors still fail")
    args = parser.parse_args(argv)
    report = {"schema": 2, "boundary": BOUNDARY, "result": "NO_EVIDENCE", "exit_code": 2,
              "attempts": [], "binaries": {}, "error": None, "confirmed_failures": [],
              "mode": "report-only" if args.report_only else "strict"}
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
        confirmed = None
        for number in range(1, config["max_attempts"] + 1):
            for role, binary in (("main", main_binary), ("pr", pr_binary)):
                gate._check_binary(binary, report["binaries"][role]["sha256"])
            at = {"number": number, "result": "NO_EVIDENCE"}; report["attempts"].append(at)
            try:
                at.update(paired(main_binary, pr_binary, args.outdir / f"attempt-{number}", config, deadline))
            finally:
                for role, binary in (("main", main_binary), ("pr", pr_binary)):
                    gate._check_binary(binary, report["binaries"][role]["sha256"])
            over = [row["scenario"] for row in at["scenarios"] if row["verdict"] == "main-over-ceiling"]
            if over:
                # This runner cannot judge the ceiling: infrastructure/no evidence, never a PR regression.
                raise gate.NoEvidence("main exceeds the absolute ceiling on this runner: " + ", ".join(over))
            at["result"] = "FAIL" if at["failures"] else "PASS"
            found = {row["scenario"] for row in at["failures"]}
            # A remeasurement confirms a finding only by regressing the same scenario.
            confirmed = found if confirmed is None else confirmed & found
            report["confirmed_failures"] = sorted(confirmed)
            report["result"], report["exit_code"] = ("FAIL", 1) if confirmed else ("PASS", 0)
            if not confirmed:
                break
    except (OSError, ValueError, subprocess.SubprocessError, gate.NoEvidence, probe.NoEvidence) as error:
        report["result"], report["exit_code"], report["error"] = "NO_EVIDENCE", 2, str(error)
    report["duration_seconds"] = time.monotonic() - started
    report["process_exit_code"] = (0 if args.report_only and report["result"] == "FAIL"
                                   else report["exit_code"])
    if created:
        (args.outdir / "result.json").write_text(json.dumps(report, indent=2) + "\n")
    if args.summary:
        with args.summary.open("a") as stream:
            stream.write(markdown(report))
    print(f"{report['result']}: same-runner input A/B")
    return report["process_exit_code"]


if __name__ == "__main__":
    sys.exit(main())
