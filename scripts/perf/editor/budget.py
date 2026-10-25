#!/usr/bin/env python3
"""Strict Linux editor CPU-cost gate (#1639), measured under Xvfb software rendering.

check SUMMARY BASELINE [--report OUT] checks each median and nearest-rank p90.
update BASELINE SUMMARY --output PATH proposes an equal-or-tighter baseline.
ratchet OLD NEW checks a proposed baseline against the protected baseline.

Exit 0 means pass; 1 means regression/relaxation; 2 means malformed/no evidence.
These measurements do not establish native display latency. Large Live Preview
support is separately validated; its new timings have no invented static baseline. No environment variables can change the gate's policy.
"""

import argparse
import copy
import json
import math
import os
import sys
import tempfile
from pathlib import Path
from statistics import median
from types import MappingProxyType


SCHEMA = 1
BOUNDARY = "xvfb-software-post-frame"
POLICY = MappingProxyType({"relative": 0.35, "absolute_ms": 25.0, "edit_floor_ms": 16.0})
FIXTURES = ("tiny.md", "sections-460.md", "one-mib.md", "novel-1200k.md")
FRAME_METRICS = ("open_first_ms", "input_frame_ms", "scroll_frame_ms")
SCENARIOS = {
    **{f"source:{name}": FRAME_METRICS for name in FIXTURES},
    "live:tiny.md": (*FRAME_METRICS, "open_full_ms"),
    "live:raw-tail-60k.md": (*FRAME_METRICS, "open_full_ms"),
}
SUPPORT_SCENARIOS = {f"live:{name}": (*FRAME_METRICS, "open_full_ms")
                     for name in FIXTURES[1:]}
UNSUPPORTED = ()
METRIC_KEYS = frozenset(f"{scenario}/{metric}"
                        for scenario, metrics in SCENARIOS.items() for metric in metrics)
STATISTICS = ("median_ms", "p90_ms")


class RelaxationError(ValueError):
    """A well-formed proposed baseline weakens at least one statistic."""


def _keys(value, expected, label):
    if not isinstance(value, dict):
        raise ValueError(f"{label}: expected an object")
    missing, extra = set(expected) - value.keys(), value.keys() - set(expected)
    if missing or extra:
        raise ValueError(f"{label}: missing {sorted(missing)!r}; extra {sorted(extra, key=str)!r}")


def _header(value):
    if type(value["schema"]) is not int or value["schema"] != SCHEMA:
        raise ValueError("schema must be integer 1")
    if value["boundary"] != BOUNDARY:
        raise ValueError(f"boundary must be {BOUNDARY!r}")


def _number(value, label):
    if type(value) not in (int, float):
        raise ValueError(f"{label}: expected a finite nonnegative number (not a boolean)")
    try:
        valid = math.isfinite(value) and value >= 0
    except OverflowError:
        valid = False
    if not valid:
        raise ValueError(f"{label}: expected a finite nonnegative number")


def _validate_metrics(metrics):
    _keys(metrics, METRIC_KEYS, "metrics")
    for key, stats in metrics.items():
        _keys(stats, STATISTICS, key)
        for statistic, value in stats.items():
            _number(value, f"{key}/{statistic}")
        if stats["p90_ms"] < stats["median_ms"]:
            raise ValueError(f"{key}: p90_ms must be >= median_ms")


def aggregate(summary):
    """Validate raw evidence; return {scenario/metric: {median_ms, p90_ms}}.

    All six samples must be valid, including the discarded first warmup. The
    nearest-rank p90 of five measured values is their maximum. Inputs are unmodified.
    Evidence entries are opaque provenance supplied by the collector.
    """
    _keys(summary, ("schema", "boundary", "samples", "support", "unsupported", "evidence"), "summary")
    _header(summary)
    unsupported = summary["unsupported"]
    if (not isinstance(unsupported, list)
            or not all(isinstance(item, str) for item in unsupported)
            or len(unsupported) != len(UNSUPPORTED) or set(unsupported) != set(UNSUPPORTED)):
        raise ValueError(f"unsupported must contain exactly {list(UNSUPPORTED)!r}")
    if not isinstance(summary["evidence"], list) or not summary["evidence"]:
        raise ValueError("no evidence: expected a nonempty evidence list")
    aggregate_support(summary)
    _keys(summary["samples"], SCENARIOS, "samples")
    result = {}
    for scenario, metrics in SCENARIOS.items():
        row = summary["samples"][scenario]
        _keys(row, metrics, scenario)
        for metric in metrics:
            samples = row[metric]
            key = f"{scenario}/{metric}"
            if not isinstance(samples, list) or len(samples) != 6:
                raise ValueError(f"{key}: expected exactly 6 samples (1 warmup + 5 measured)")
            for index, sample in enumerate(samples):
                _number(sample, f"{key}[{index}]")
            measured = samples[1:]
            result[key] = {"median_ms": float(median(measured)), "p90_ms": float(max(measured))}
    return result


def validate_baseline(baseline):
    """Validate the full immutable baseline contract; return it without mutation."""
    _keys(baseline, ("schema", "boundary", "policy", "metrics"), "baseline")
    _header(baseline)
    _keys(baseline["policy"], POLICY, "policy")
    for key, expected in POLICY.items():
        _number(baseline["policy"][key], f"policy/{key}")
        if baseline["policy"][key] != expected:
            raise ValueError(f"policy/{key} must equal {expected}")
    _validate_metrics(baseline["metrics"])
    return baseline


def aggregate_support(summary, required=False):
    """Measured full-LP scenarios, outside the unchanged 20-key static baseline.

    A retry may explicitly omit them because they are not budget comparisons.
    The gate requires every support scenario on attempt 1; missing/limited LP
    is never a valid regression or a successful support claim.
    """
    rows = summary.get("support")
    if rows is None:
        if required:
            raise ValueError("missing full Live Preview support measurements")
        return {}
    _keys(rows, SUPPORT_SCENARIOS, "support")
    result = {}
    for scenario, metrics in SUPPORT_SCENARIOS.items():
        _keys(rows[scenario], metrics, scenario)
        for metric in metrics:
            samples = rows[scenario][metric]
            if not isinstance(samples, list) or len(samples) != 6:
                raise ValueError(f"{scenario}/{metric}: expected 6 support samples")
            for value in samples:
                _number(value, f"{scenario}/{metric}")
            result[f"{scenario}/{metric}"] = {
                "median_ms": float(median(samples[1:])), "p90_ms": float(max(samples[1:]))}
    return result


def verdict(baseline, observed):
    """Return structured failures for a full baseline and aggregate(summary) metrics."""
    validate_baseline(baseline)
    _validate_metrics(observed)
    failures = []
    for key in sorted(METRIC_KEYS):
        for statistic in STATISTICS:
            base = baseline["metrics"][key][statistic]
            limit = max(base * (1 + POLICY["relative"]), base + POLICY["absolute_ms"])
            if key.endswith("/input_frame_ms"):
                limit = max(limit, POLICY["edit_floor_ms"])
            value = observed[key][statistic]
            if value > limit:
                failures.append({"metric": key, "statistic": statistic, "baseline_ms": base,
                                 "observed_ms": value, "limit_ms": limit})
    return failures


def tighten(old, observed):
    """Return a detached proposed full baseline only if no statistic increases.

    Both arguments are full baseline documents; update constructs the candidate
    solely from validated summary aggregates and the old immutable contract.
    """
    validate_baseline(old)
    validate_baseline(observed)
    increases = [f"{key}/{stat}: {old['metrics'][key][stat]} -> {observed['metrics'][key][stat]}"
                 for key in sorted(METRIC_KEYS) for stat in STATISTICS
                 if observed["metrics"][key][stat] > old["metrics"][key][stat]]
    if increases:
        raise RelaxationError("baseline relaxation refused: " + "; ".join(increases))
    return copy.deepcopy(observed)


def _load(path):
    def unique_object(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError(f"duplicate JSON key: {key}")
            result[key] = value
        return result

    return json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=unique_object)


def _write(path, value):
    """Serialize before touching output, then replace atomically in its directory."""
    encoded = json.dumps(value, indent=2, sort_keys=True, allow_nan=False) + "\n"
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=path.parent,
                                         prefix=f".{path.name}.", delete=False) as stream:
            temporary = Path(stream.name)
            stream.write(encoded)
        os.replace(temporary, path)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def _scope():
    print(f"Scope: {BOUNDARY} (Xvfb software CPU cost; native display latency unproven)")
    for scenario in SUPPORT_SCENARIOS:
        print(f"MEASURED WITHOUT STATIC BUDGET: {scenario} (#1662 A/B follows)")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    check = commands.add_parser("check")
    check.add_argument("summary", type=Path)
    check.add_argument("baseline", type=Path)
    check.add_argument("--report", type=Path)
    update = commands.add_parser("update")
    update.add_argument("baseline", type=Path)
    update.add_argument("summary", type=Path)
    update.add_argument("--output", type=Path, required=True)
    ratchet = commands.add_parser("ratchet")
    ratchet.add_argument("old", type=Path)
    ratchet.add_argument("new", type=Path)
    args = parser.parse_args(argv)
    _scope()
    try:
        if args.command == "ratchet":
            tighten(_load(args.old), _load(args.new))
            print("PASS: baseline statistics did not increase")
            return 0
        baseline = validate_baseline(_load(args.baseline))
        summary = _load(args.summary)
        observed = aggregate(summary)
        if args.command == "update":
            candidate = {"schema": SCHEMA, "boundary": BOUNDARY,
                         "policy": dict(POLICY), "metrics": observed}
            proposed = tighten(baseline, candidate)
            _write(args.output, proposed)
            print(f"PASS: equal-or-tighter proposal written to {args.output}")
            return 0
        failures = verdict(baseline, observed)
        for key, stats in sorted(observed.items()):
            print(f"{key}: median {stats['median_ms']:.3f} ms; p90 {stats['p90_ms']:.3f} ms")
        for failure in failures:
            print(f"FAILED: {failure['metric']}/{failure['statistic']} "
                  f"{failure['observed_ms']:.3f} > {failure['limit_ms']:.3f} ms")
        if args.report:
            if args.report.resolve() in (args.summary.resolve(), args.baseline.resolve()):
                raise ValueError("report must not overwrite a summary or baseline")
            _write(args.report, {"schema": SCHEMA, "boundary": BOUNDARY,
                                 "scope": "Xvfb software CPU cost; native display latency unproven",
                                 "unsupported": list(UNSUPPORTED), "evidence": summary["evidence"],
                                 "metrics": observed, "support_metrics": aggregate_support(summary),
                                 "failures": failures})
        print("FAIL: editor latency budget exceeded" if failures else "PASS: editor latency budget")
        return 1 if failures else 0
    except RelaxationError as error:
        print(f"REFUSED: {error}")
        return 1
    except (OSError, ValueError, OverflowError) as error:
        print(f"NO EVIDENCE: {error}")
        return 2


if __name__ == "__main__":
    sys.exit(main())
