#!/usr/bin/env python3
"""#2094 speed budget collector: the four Stable metrics from one binary.

App metrics reuse the #1639/#1662 editor probe (``probe.run_once``): an isolated
home/state/vault per launch, Xvfb, endpoints from the product's post-frame
diagnostic on Linux ``CLOCK_MONOTONIC``, with positive controls. Six
interleaved rounds, the first one a warmup; each round launches:

- ``tiny.md`` (3 KiB) in Source: ``cold_start_ms`` = process launch to the first
  painted content frame of the note (fresh profile, OS file cache warm), and
  ``typing_ms`` = one key to the post-frame of its exact document generation,
  with the changed buffer and window pixels verified outside the interval;
- ``ten-mib.md`` (10 MiB) in Source: launch to its first painted content frame.
  ``open_10mib_ms`` = that minus the same round's tiny launch, so the startup
  shared by both launches cancels out and the note's own open cost remains.

``search_10k_ms`` comes from the ``synthetic_10k_query_latency`` Rust probe
(``crates/okilum-core/tests/search_bench.rs``): the slowest query's median of
30 timed palette calls on a 10,000-note vault. Its JSON is passed in.

Missing evidence is never a pass: a failed control marks that metric
``no_evidence`` and the judge exits 2 whatever the mode.
"""

import argparse
import hashlib
import json
import math
from pathlib import Path
import statistics
import sys
import time

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "editor"))

import ab  # noqa: E402  (hardware receipt only)
import notes  # noqa: E402
import probe  # noqa: E402

ROUNDS = 6  # round 0 is the warmup
DEADLINE_SECONDS = 900
# A slow 10 MiB open is a measured budget failure, not missing evidence: wait
# up to a minute for its first frame (the editor probe's default is 12 s).
LARGE_OPEN_LIMIT_SECONDS = 60
SEARCH_RUNS = 30
SEARCH_NOTES = 10_000
APP_METRICS = ("cold_start_ms", "typing_ms", "open_10mib_ms")


def stats(samples):
    ordered = sorted(samples)
    return {"median_ms": statistics.median(ordered),
            "p90_ms": ordered[math.ceil(.9 * len(ordered)) - 1], "samples_ms": samples}


def search_metric(report):
    """The slowest query's median; ValueError when the probe's controls are absent."""
    if (report.get("schema") != 1 or report.get("notes") != SEARCH_NOTES
            or report.get("runs") != SEARCH_RUNS or not report.get("queries")):
        raise ValueError("search report lacks the 10,000-note, 30-run probe")
    queries = {}
    for row in report["queries"]:
        samples = row.get("samples_ms", [])
        if len(samples) != SEARCH_RUNS or not all(
                type(x) in (int, float) and 0 <= x < math.inf for x in samples):
            raise ValueError(f"search query {row.get('query')!r} has invalid samples")
        queries[row["query"]] = dict(stats(samples), first_ms=row["first_ms"], hits=row["hits"])
    worst = max(queries, key=lambda q: queries[q]["median_ms"])
    return {"median_ms": queries[worst]["median_ms"], "p90_ms": queries[worst]["p90_ms"],
            "slowest_query": worst, "queries": queries,
            "index": {key: report[key] for key in ("scan_ms", "build_ms", "open_ms", "source_bytes")}}


def app_metrics(binary, out, files, deadline, run_once=probe.run_once):
    """Interleaved tiny/10 MiB launches. Returns (metrics, evidence, errors)."""
    raw = {"cold_start_ms": [], "typing_ms": [], "open_10mib_ms": [], "open_10mib_absolute_ms": []}
    evidence, errors = [], {}
    for rnd in range(ROUNDS):
        tiny = big = None
        if "tiny" not in errors:
            try:
                tiny, proof = run_once(binary, out / "tiny" / f"round{rnd}", notes.SMALL, False, rnd,
                                       deadline, input_only=True, keep_open=True,
                                       source=files[notes.SMALL])
                evidence.append(proof)
            except (probe.NoEvidence, OSError, ValueError, KeyError) as error:
                errors["tiny"] = f"round {rnd}: {error}"
        if "large" not in errors:
            try:
                big, proof = run_once(binary, out / "ten-mib" / f"round{rnd}", notes.LARGE, False, rnd,
                                      deadline, input_only=True, open_only=True,
                                      source=files[notes.LARGE],
                                      open_limit=LARGE_OPEN_LIMIT_SECONDS)
                evidence.append(proof)
            except (probe.NoEvidence, OSError, ValueError, KeyError) as error:
                errors["large"] = f"round {rnd}: {error}"
        print(f"round {rnd}{' warmup' if rnd == 0 else ''}: tiny={tiny} ten-mib={big}", flush=True)
        if rnd == 0:
            continue
        if tiny:
            raw["cold_start_ms"].append(tiny["open_first_ms"])
            raw["typing_ms"].append(tiny["input_frame_ms"])
        if tiny and big:
            raw["open_10mib_absolute_ms"].append(big["open_first_ms"])
            raw["open_10mib_ms"].append(big["open_first_ms"] - tiny["open_first_ms"])
    metrics = {}
    reasons = {"cold_start_ms": errors.get("tiny"), "typing_ms": errors.get("tiny"),
               "open_10mib_ms": errors.get("tiny") or errors.get("large")}
    for name in APP_METRICS:
        if reasons[name] or len(raw[name]) != ROUNDS - 1:
            metrics[name] = {"status": "no_evidence", "reason": reasons[name] or "missing rounds"}
        else:
            metrics[name] = dict(stats(raw[name]), status="measured")
    if metrics["open_10mib_ms"]["status"] == "measured":
        metrics["open_10mib_ms"]["absolute"] = stats(raw["open_10mib_absolute_ms"])
    return metrics, evidence, errors


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("binary", type=Path)
    parser.add_argument("outdir", type=Path)
    parser.add_argument("--search-report", type=Path, required=True)
    parser.add_argument("--source-commit", required=True)
    args = parser.parse_args(argv)
    binary, out = args.binary.resolve(strict=True), args.outdir.resolve()
    if out.exists():
        raise SystemExit(f"output already exists: {out}")
    out.mkdir(parents=True)
    summary = {"schema": 1, "source_commit": args.source_commit,
               "binary": {"path": str(binary), "sha256": sha256(binary)},
               "host": ab.hardware(), "platform": "linux-x11-xvfb",
               "boundary": "Xvfb software post-frame (no GPU completion or scanout)",
               "rounds": {"warmup": 1, "measured": ROUNDS - 1}, "metrics": {}, "evidence": []}
    started = time.monotonic()
    try:
        files = notes.checked()
        metrics, evidence, errors = app_metrics(binary, out, files, started + DEADLINE_SECONDS)
        summary["metrics"].update(metrics)
        summary["evidence"] = evidence
        summary["errors"] = errors
        try:
            summary["metrics"]["search_10k_ms"] = dict(
                search_metric(json.loads(args.search_report.read_text())), status="measured")
        except (OSError, ValueError, KeyError, TypeError) as error:
            summary["metrics"]["search_10k_ms"] = {"status": "no_evidence", "reason": str(error)}
    finally:
        summary["duration_seconds"] = time.monotonic() - started
        (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps({k: {f: v.get(f) for f in ("status", "median_ms", "p90_ms")}
                      for k, v in summary["metrics"].items()}, indent=2))


if __name__ == "__main__":
    main()
