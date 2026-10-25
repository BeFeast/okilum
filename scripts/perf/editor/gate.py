#!/usr/bin/env python3
"""Measure one existing binary; retry one valid regression, within 295 seconds.

Exit 0: pass (possibly on retry); 1: two valid regressions; 2: no evidence.
Infrastructure failures never trigger a retry. No environment overrides policy.
"""

import argparse
import hashlib
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import time

import budget


HERE = Path(__file__).resolve().parent
TOTAL_SECONDS = 295
MAX_ATTEMPTS = 2
CLEANUP_SECONDS = 3
SCOPE = ("Xvfb software CPU/post-frame submission cost; scroll is CPU draw cost. "
         "GPU completion, scanout and native display latency are unproven.")


class NoEvidence(RuntimeError):
    pass


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def source_commit(binary):
    """Read optional build provenance, without claiming a commit from the cwd."""
    receipt = binary.parent / "BUILD.txt"
    if not receipt.exists():
        return None
    with receipt.open(encoding="utf-8") as stream:
        match = re.fullmatch(r"commit ([0-9a-f]{40})\n?", stream.readline())
    if match is None:
        raise NoEvidence("invalid source commit in binary BUILD.txt receipt")
    return match[1]


def _signal_group(proc, sig):
    try:
        os.killpg(proc.pid, sig)
    except ProcessLookupError:
        pass


def _stop_group(proc, deadline):
    """Allow collector/app cleanup, then kill the whole session's process group.

    Kill the group even if the collector exits on TERM: an app may ignore TERM
    after its parent has exited. All waits share the gate's original deadline.
    """
    _signal_group(proc, signal.SIGTERM)
    grace = max(0, min(1.0, deadline - time.monotonic()))
    try:
        proc.wait(timeout=grace)
    except subprocess.TimeoutExpired:
        pass
    finally:
        _signal_group(proc, signal.SIGKILL)
    proc.wait(timeout=max(0, deadline - time.monotonic()))


def run_collector(command, log_path, deadline):
    """Return (collector exit, timed out); logs stay outside collector-owned dirs."""
    remaining = deadline - time.monotonic() - CLEANUP_SECONDS
    if remaining <= 0:
        raise NoEvidence("total deadline exhausted before collector launch")
    with log_path.open("wb") as log:
        proc = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT,
                                start_new_session=True)
        try:
            remaining = deadline - time.monotonic() - CLEANUP_SECONDS
            if remaining <= 0:
                raise subprocess.TimeoutExpired(command, 0)
            return proc.wait(timeout=remaining), False
        except subprocess.TimeoutExpired:
            _stop_group(proc, deadline)
            return proc.returncode, True
        except BaseException:
            _stop_group(proc, deadline)
            raise


def _check_binary(binary, expected):
    observed = sha256(binary)
    if observed != expected:
        raise NoEvidence("binary SHA256 changed; measurements cannot be compared")
    return observed


def _attempt(binary, out, baseline, expected_sha, number, deadline, report):
    attempt_dir = out / f"attempt-{number}"
    attempt = {"number": number, "directory": str(attempt_dir),
               "collector_exit": None, "timed_out": False,
               "exit_code": 2, "result": "NO_EVIDENCE",
               "metrics": {}, "support_metrics": {}, "failures": [], "error": None,
               "binary_sha256_before": None, "binary_sha256_after": None}
    report["attempts"].append(attempt)
    started = time.monotonic()
    try:
        attempt["binary_sha256_before"] = _check_binary(binary, expected_sha)
        if deadline - time.monotonic() <= CLEANUP_SECONDS:
            raise NoEvidence("total deadline exhausted before attempt")
        command = [sys.executable, str(HERE / "probe.py")]
        if number == 2:
            command.append("--budgeted-only")
        command.extend([str(binary), str(attempt_dir)])
        try:
            code, timed_out = run_collector(command, out / f"attempt-{number}.collector.log",
                                           deadline)
            attempt["collector_exit"] = code
            attempt["timed_out"] = timed_out
        finally:
            attempt["binary_sha256_after"] = _check_binary(binary, expected_sha)
        if timed_out or time.monotonic() >= deadline:
            raise NoEvidence("collector exceeded the shared 295-second deadline")
        if code != 0:
            raise NoEvidence(f"collector exited {code}; see attempt-{number}.collector.log")
        raw = budget._load(attempt_dir / "summary.json")
        attempt["metrics"] = budget.aggregate(raw)
        attempt["support_metrics"] = budget.aggregate_support(raw, required=number == 1)
        attempt["failures"] = budget.verdict(baseline, attempt["metrics"])
        attempt["evidence"] = raw["evidence"]
        if time.monotonic() >= deadline:
            raise NoEvidence("evidence validation exceeded the shared 295-second deadline")
        attempt["result"] = "FAIL" if attempt["failures"] else "PASS"
        attempt["exit_code"] = 1 if attempt["failures"] else 0
    except (OSError, ValueError, OverflowError, subprocess.SubprocessError, NoEvidence) as error:
        attempt["error"] = str(error)
    finally:
        attempt["duration_seconds"] = time.monotonic() - started
    return attempt


def _cell(value):
    return str(value).replace("|", "\\|").replace("\r", " ").replace("\n", " ")


def markdown(report):
    lines = ["", "## Editor latency gate", "", f"**{report['result']}** "
             f"(exit {report['exit_code']})", "", SCOPE, "",
             f"Boundary: `{budget.BOUNDARY}`.", "",
             f"Binary SHA256: `{report['binary_sha256'] or 'unavailable'}`.", "",
             f"Source commit (binary BUILD.txt receipt): "
             f"`{report['source_commit'] or 'unavailable'}`.", "",
             "Policy: measure the existing binary; retry exactly once only after a valid "
             "budget regression. Both attempts share 295 seconds. Thresholds are unchanged: "
             "max(baseline × 1.35, baseline + 25 ms), with a 16 ms input floor.", ""]
    if report["warning"]:
        lines.extend([f"**Warning:** {report['warning']}", ""])
    if report["error"]:
        lines.extend([f"NO EVIDENCE: {_cell(report['error'])}", ""])
    for attempt in report["attempts"]:
        lines.extend([f"### Attempt {attempt['number']}: {attempt['result']}", "",
                      f"Collector exit: {attempt['collector_exit']}; "
                      f"gate exit: {attempt['exit_code']}.", ""])
        if attempt["error"]:
            lines.extend([f"NO EVIDENCE: {_cell(attempt['error'])}", ""])
        if attempt["metrics"]:
            lines.extend(["| Metric | Observed median (ms) | Observed p90 (ms) | Failures |",
                          "| --- | ---: | ---: | --- |"])
            for key, stats in sorted(attempt["metrics"].items()):
                failures = "; ".join(
                    f"{row['statistic']}: {row['observed_ms']:.3f} > "
                    f"{row['limit_ms']:.3f} ms (baseline {row['baseline_ms']:.3f})"
                    for row in attempt["failures"] if row["metric"] == key)
                lines.append(f"| {_cell(key)} | {stats['median_ms']:.3f} | "
                             f"{stats['p90_ms']:.3f} | {failures or 'none'} |")
            lines.append("")
        if attempt["support_metrics"]:
            lines.extend(["### Full Live Preview support measurements", "",
                          "All support cases require complete, non-limited matching projection frames, "
                          "actual input and scroll controls. New timings are measured without a static "
                          "budget; same-runner A/B belongs to #1662.", "",
                          "| Metric | Median (ms) | p90 (ms) |",
                          "| --- | ---: | ---: |"])
            for key, stats in sorted(attempt["support_metrics"].items()):
                lines.append(f"| {_cell(key)} | {stats['median_ms']:.3f} | {stats['p90_ms']:.3f} |")
            lines.append("")
        elif attempt["number"] == 2:
            lines.extend(["Full-LP support is retained from attempt 1; this retry remeasures "
                          "only the 20 static-budget metrics.", ""])
    lines.extend(["Unsupported (unmeasured; no PASS claim): "
                  + ", ".join(f"`{name}`" for name in budget.UNSUPPORTED)
                  + ("." if budget.UNSUPPORTED else "none; large LP support requires the attempt-1 measurements above."), ""])
    return "\n".join(lines)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("outdir", type=Path)
    parser.add_argument("baseline", type=Path)
    parser.add_argument("--summary", type=Path,
                        default=os.environ.get("GITHUB_STEP_SUMMARY") or None)
    args = parser.parse_args(argv)
    started = time.monotonic()
    deadline = started + TOTAL_SECONDS
    report = {"schema": 1, "boundary": budget.BOUNDARY, "scope": SCOPE,
              "policy": {"max_attempts": MAX_ATTEMPTS, "total_seconds": TOTAL_SECONDS,
                         "retry_only": "valid_budget_regression", "budget": dict(budget.POLICY)},
              "binary": str(args.binary), "binary_sha256": None, "source_commit": None,
              "baseline": str(args.baseline), "unsupported": list(budget.UNSUPPORTED),
              "attempts": [], "result": "NO_EVIDENCE", "exit_code": 2,
              "warning": None, "error": None}
    out = args.outdir.absolute()
    created = False
    summary_path = None
    try:
        if out.exists() or out.is_symlink():
            raise NoEvidence(f"output already exists: {out}")
        out.mkdir(parents=True)
        created = True
        out = out.resolve()
        if args.summary is not None:
            candidate = args.summary.resolve()
            if candidate in (args.binary.resolve(), args.baseline.resolve(),
                             HERE / "probe.py", HERE / "gate.py", HERE / "budget.py",
                             args.binary.resolve().parent / "BUILD.txt",
                             out, out / "result.json"):
                raise NoEvidence("summary must not overwrite an input or result.json")
            if candidate.is_relative_to(out) and candidate.relative_to(out).parts[0].startswith("attempt-"):
                raise NoEvidence("summary must not overwrite collector evidence")
            summary_path = candidate
        # Baseline errors must be detected before any application can launch.
        baseline = budget.validate_baseline(budget._load(args.baseline))
        binary = args.binary.resolve(strict=True)
        report["binary"] = str(binary)
        report["source_commit"] = source_commit(binary)
        report["binary_sha256"] = sha256(binary)
        for number in range(1, MAX_ATTEMPTS + 1):
            attempt = _attempt(binary, out, baseline, report["binary_sha256"],
                               number, deadline, report)
            report["result"], report["exit_code"] = attempt["result"], attempt["exit_code"]
            report["error"] = attempt["error"]
            if attempt["exit_code"] != 1:
                if number == 2 and attempt["exit_code"] == 0:
                    report["warning"] = ("Attempt 1 exceeded the budget; the single permitted "
                                         "retry of the same binary passed.")
                break
    except (OSError, ValueError, OverflowError, NoEvidence) as error:
        report["result"], report["exit_code"], report["error"] = "NO_EVIDENCE", 2, str(error)

    # Even a bad baseline or collector failure leaves a machine-readable report.
    # Never touch an output directory that this invocation did not create.
    report["duration_seconds"] = time.monotonic() - started
    if created:
        try:
            budget._write(out / "result.json", report)
        except (OSError, ValueError) as error:
            report["result"], report["exit_code"], report["error"] = "NO_EVIDENCE", 2, str(error)
        if summary_path is not None:
            try:
                with summary_path.open("a", encoding="utf-8") as stream:
                    stream.write(markdown(report))
            except (OSError, ValueError) as error:
                report["result"], report["exit_code"], report["error"] = "NO_EVIDENCE", 2, str(error)
                try:
                    budget._write(out / "result.json", report)
                except OSError:
                    pass
    print(f"{report['result']}: editor latency gate"
          + (f"; {report['error']}" if report["error"] else ""))
    return report["exit_code"]


if __name__ == "__main__":
    sys.exit(main())
