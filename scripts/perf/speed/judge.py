#!/usr/bin/env python3
"""#2094 speed budget verdict for one collector summary.

Each metric's median gets two verdicts against ``budgets.json``:

- ``product``: the strict budget Oleg set on 31.10 (300 / 16 / 1000 / 100 ms).
  It is decided on reference machines; on a hosted runner it is reported, never
  enforced, because a 4-vCPU Xvfb software renderer is not one.
- ``hosted``: the regression ceiling for this hosted runner class, calibrated
  from measured runs with the margin recorded in ``budgets.json``.

Result: ``NO_EVIDENCE`` when any metric lacks valid samples, otherwise ``FAIL``
when a median exceeds its hosted ceiling, otherwise ``PASS``. Exit codes:
2 for ``NO_EVIDENCE`` in every mode; 1 for ``FAIL`` only with ``--enforce``;
0 otherwise (report-only is the default; enforcing it for Stable promotion
needs Oleg's decision).
"""

import argparse
import json
import math
from pathlib import Path
import sys

HERE = Path(__file__).resolve().parent
METRICS = ("cold_start_ms", "typing_ms", "open_10mib_ms", "search_10k_ms")
LABELS = {"cold_start_ms": "Cold start (launch → first note frame)",
          "typing_ms": "Typing (key → post-frame)",
          "open_10mib_ms": "Open 10 MiB note (over tiny control)",
          "search_10k_ms": "Search 10k notes (slowest query median)"}


def load_budgets(path):
    budgets = json.loads(Path(path).read_text())
    if budgets.get("schema") != 1 or tuple(budgets.get("metrics", {})) != METRICS:
        raise ValueError("budgets.json must list exactly the four #2094 metrics in order")
    for name, row in budgets["metrics"].items():
        for key in ("product_budget_ms", "hosted_limit_ms"):
            value = row.get(key)
            if type(value) not in (int, float) or not 0 < value < math.inf:
                raise ValueError(f"{name}.{key} must be a positive number")
        if row["hosted_limit_ms"] < row["product_budget_ms"]:
            raise ValueError(f"{name}: hosted ceiling below the product budget")
    return budgets


def judge(summary, budgets):
    rows, missing, over = [], [], []
    for name in METRICS:
        metric = summary.get("metrics", {}).get(name, {})
        budget = budgets["metrics"][name]
        value = metric.get("median_ms")
        if metric.get("status") != "measured" or type(value) not in (int, float) or not math.isfinite(value):
            missing.append(name)
            rows.append({"metric": name, "status": "no_evidence",
                         "reason": metric.get("reason", "metric absent")})
            continue
        row = {"metric": name, "status": "measured", "median_ms": value,
               "p90_ms": metric.get("p90_ms"),
               "product_budget_ms": budget["product_budget_ms"],
               "hosted_limit_ms": budget["hosted_limit_ms"],
               "product": "pass" if value < budget["product_budget_ms"] else "over",
               "hosted": "pass" if value <= budget["hosted_limit_ms"] else "over"}
        if row["hosted"] == "over":
            over.append(name)
        rows.append(row)
    result = "NO_EVIDENCE" if missing else "FAIL" if over else "PASS"
    return {"schema": 1, "result": result, "rows": rows, "over_hosted": over,
            "over_product": [r["metric"] for r in rows if r.get("product") == "over"],
            "no_evidence": missing, "source_commit": summary.get("source_commit"),
            "host": summary.get("host"), "binary": summary.get("binary")}


def markdown(report, enforce):
    mode = "enforced" if enforce else "report-only"
    host = report.get("host") or {}
    lines = [f"### Speed budget (#2094): **{report['result']}** ({mode})", "",
             f"Source `{report.get('source_commit')}`, CPU {', '.join(host.get('cpu_models', []))}"
             f" / {host.get('available_cpus')} vCPU, Xvfb software rendering.", "",
             "| metric | median | p90 | product budget | hosted ceiling |",
             "|---|---:|---:|---:|---:|"]
    for row in report["rows"]:
        if row["status"] != "measured":
            lines.append(f"| {LABELS[row['metric']]} | NO EVIDENCE: {row['reason']} | | | |")
            continue
        mark = {"pass": "✓", "over": "✗"}
        lines.append(f"| {LABELS[row['metric']]} | {row['median_ms']:.1f} ms | {row['p90_ms']:.1f} ms"
                     f" | {row['product_budget_ms']} ms {mark[row['product']]}"
                     f" | {row['hosted_limit_ms']} ms {mark[row['hosted']]} |")
    lines += ["", "The product budget is judged on reference machines; on this hosted runner it is"
              " informational. Only the hosted ceiling decides FAIL, and FAIL blocks nothing"
              " unless the workflow runs with `enforce`."]
    return "\n".join(lines) + "\n"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("summary", type=Path)
    parser.add_argument("--budgets", type=Path, default=HERE / "budgets.json")
    parser.add_argument("--report", type=Path)
    parser.add_argument("--markdown", type=Path)
    parser.add_argument("--enforce", action="store_true",
                        help="exit 1 on a hosted-ceiling FAIL (off by default, see docstring)")
    args = parser.parse_args(argv)
    try:
        report = judge(json.loads(args.summary.read_text()), load_budgets(args.budgets))
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"NO EVIDENCE: {error}", file=sys.stderr)
        return 2
    report["enforced"] = args.enforce
    text = markdown(report, args.enforce)
    print(text)
    if args.report:
        args.report.write_text(json.dumps(report, indent=2) + "\n")
    if args.markdown:
        with args.markdown.open("a") as handle:
            handle.write(text)
    if report["result"] == "NO_EVIDENCE":
        return 2
    return 1 if report["result"] == "FAIL" and args.enforce else 0


if __name__ == "__main__":
    sys.exit(main())
