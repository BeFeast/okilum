#!/usr/bin/env python3
"""Push committed HEAD to the public mirror and return an exact-source verdict."""
import argparse
import json
import re
import subprocess
import sys
import time
from datetime import datetime
from pathlib import Path

REPO = "BeFeast/okilum"
WORKFLOW = "agent-checks.yml"
INFRA_STEPS = {
    "Set up job", "Record exact source and runner", "Install system libraries",
    "Verify pinned Rust toolchain", "Restore Rust downloads and target cache",
    "Fetch locked public dependencies", "Save Rust downloads and target cache",
    "Prepare isolated Python test dependencies",
}


def run(*args, **kwargs):
    kwargs.setdefault("timeout", 120)
    return subprocess.run(args, text=True, check=True, **kwargs)


def output(*args):
    return run(*args, stdout=subprocess.PIPE).stdout.strip()


def gh_json(*args):
    return json.loads(output("gh", *args))


def verdict(info, sha, benchmark=False, publication_controls=False):
    """Missing/mismatched/partial/cancelled work never counts as a verdict."""
    if info.get("headSha") != sha or info.get("status") != "completed":
        return 75
    jobs = info.get("jobs", [])
    if len(jobs) != 1 or jobs[0].get("name") != "linux":
        return 75
    steps = jobs[0].get("steps", [])
    required = {"Prepare and verify pinned vendor and comrak", "Lint every workspace target offline",
                "Build Linux desktop offline", "Run complete okilum-core suite offline", "Run Linux Rust suites offline",
                "Run CI, releases and updater Python suites"}
    if publication_controls:
        required.add("Compare editor publication with pinned main on this host")
    if benchmark:
        required |= {"Warm build Linux desktop offline", "Warm complete okilum-core suite offline",
                     "Warm Linux Rust suites offline"}
    if info.get("conclusion") == "success":
        passed = {s["name"] for s in steps if s.get("conclusion") == "success"}
        return 0 if required <= passed and jobs[0].get("conclusion") == "success" else 75
    if info.get("conclusion") != "failure":
        return 75
    failed = [s for s in steps if s.get("conclusion") == "failure"]
    if not failed or any(s["name"] in INFRA_STEPS or "actions/checkout@" in s["name"] for s in failed):
        return 75
    return 1


def find_run(branch, sha, event):
    # gh run list --workflow 404s until the workflow exists on the default
    # branch. The API works during bootstrap and identifies the exact file.
    runs = gh_json("api", "--method", "GET", f"repos/{REPO}/actions/runs",
                   "-f", "branch=" + branch, "-f", "head_sha=" + sha,
                   "-f", "event=" + event, "-f", "per_page=100")["workflow_runs"]
    return [r for r in runs if r["head_sha"] == sha and r["head_branch"] == branch and
            r["event"] == event and r["path"] == ".github/workflows/" + WORKFLOW]


def summarize(info):
    print(f"{info['url']} attempt={info['attempt']} head={info['headSha']}")
    for job in info.get("jobs", []):
        print(f"{job['name']}: {job.get('conclusion')}")
        for step in job.get("steps", []):
            elapsed = "-"
            if step.get("startedAt") and step.get("completedAt"):
                elapsed = str(int((datetime.fromisoformat(step["completedAt"].replace("Z", "+00:00")) -
                                   datetime.fromisoformat(step["startedAt"].replace("Z", "+00:00"))).total_seconds())) + "s"
            print(f"  {step.get('conclusion', '?'):10s} {elapsed:>7s}  {step['name']}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rerun", action="store_true", help="rerun this exact SHA (also measures warm cache)")
    parser.add_argument("--dispatch", action="store_true", help="dispatch after pushing instead of using the push run")
    parser.add_argument("--timeout", type=int, default=7200, help="total wait ceiling in seconds")
    parser.add_argument("--receipt", type=Path, help="also write the final GitHub JSON receipt to this path")
    parser.add_argument("--benchmark", action="store_true", help="repeat checks with warm target on the same runner VM")
    parser.add_argument("--publication-baseline", help="public main SHA for same-host editor controls; requires --dispatch")
    args = parser.parse_args()
    if args.publication_baseline and (not args.dispatch or not re.fullmatch(r"[0-9a-f]{40}", args.publication_baseline)):
        parser.error("--publication-baseline requires --dispatch and a full lowercase SHA")
    if args.rerun and args.dispatch:
        parser.error("choose --rerun or --dispatch")
    if args.timeout <= 0:
        parser.error("--timeout must be positive")
    run("git", "rev-parse", "--show-toplevel", stdout=subprocess.DEVNULL)
    if output("git", "status", "--porcelain"):
        print("Commit the worktree before requesting a source-pinned check.", file=sys.stderr)
        return 75
    sha = output("git", "rev-parse", "HEAD")
    branch = output("git", "symbolic-ref", "--quiet", "--short", "HEAD")
    # A suffix /benchmark would conflict with the ordinary branch as a Git ref
    # file/directory. Keep benchmark refs in a sibling namespace instead.
    branch = (f"agent-checks-benchmark/{sha}/" if args.benchmark else "agent-checks/") + branch
    ref = "refs/heads/" + branch
    remote = "https://github.com/" + REPO + ".git"
    # Use the existing gh credential store without changing global git config or
    # placing a token in argv, a remote URL, a tracked file, or the runner.
    git = ["git", "-c", "credential.helper=", "-c", "credential.helper=!gh auth git-credential"]
    previous = output(*git, "ls-remote", remote, ref).split()
    expected = previous[0] if previous else ""
    run(*git, "push", "--force-with-lease=" + ref + ":" + expected,
        remote, sha + ":" + ref)
    deadline = time.monotonic() + args.timeout
    event = "workflow_dispatch" if args.dispatch else "push"
    previous_runs = set()
    if args.dispatch:
        previous_runs = {r["id"] for r in find_run(branch, sha, event)}
        dispatch = ["gh", "workflow", "run", WORKFLOW, "-R", REPO, "--ref", branch]
        if args.publication_baseline:
            dispatch += ["-f", "publication_baseline=" + args.publication_baseline]
        run(*dispatch)
    found = None
    # GitHub's run index is eventually consistent. Polling lives only inside
    # this single bounded helper process, never in an agent/thread loop.
    while time.monotonic() < deadline:
        matches = [r for r in find_run(branch, sha, event) if r["id"] not in previous_runs]
        if matches:
            found = matches[0]
            break
        time.sleep(min(5, max(0, deadline - time.monotonic())))
    if not found:
        print("No exact-head workflow run appeared before the deadline.", file=sys.stderr)
        return 75
    run_id = str(found["id"])
    attempt = None
    if args.rerun:
        # gh refuses to rerun work that has not completed yet.
        if found["status"] != "completed":
            print("The exact-head run is still active; rerun after it completes.", file=sys.stderr)
            return 75
        attempt = gh_json("run", "view", run_id, "-R", REPO, "--json", "attempt")["attempt"]
        run("gh", "run", "rerun", run_id, "-R", REPO)
        # The rerun API returns before the old completed attempt disappears.
        # Never let gh watch (or our verdict) reuse that previous result.
        while time.monotonic() < deadline:
            current = gh_json("run", "view", run_id, "-R", REPO, "--json", "attempt,status")
            if current["attempt"] > attempt:
                break
            time.sleep(min(5, max(0, deadline - time.monotonic())))
        else:
            return 75
    # ~20 concurrent helpers may share one user's hourly API quota.
    run("gh", "run", "watch", run_id, "-R", REPO, "--compact", "--interval", "60",
        timeout=max(1, deadline - time.monotonic()))
    info = gh_json("run", "view", run_id, "-R", REPO, "--json",
                   "headSha,status,conclusion,jobs,url,attempt,createdAt,updatedAt")
    if args.receipt:
        args.receipt.write_text(json.dumps(info, indent=2) + "\n")
    summarize(info)
    if attempt is not None and info["attempt"] <= attempt:
        return 75
    result = verdict(info, sha, args.benchmark, bool(args.publication_baseline))
    if result:
        logs = subprocess.run(["gh", "run", "view", run_id, "-R", REPO, "--log-failed"],
                              text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=120)
        tail = logs.stdout.splitlines()[-100:]
        print("\nFailed-step log tail:\n" + "\n".join(tail))
        if logs.returncode:
            return 75
        # Shell exits for OOM/killed processes are infrastructure, not a code verdict.
        if any(marker in logs.stdout for marker in (
                "exit code 137", "No space left on device", "Cannot allocate memory",
                "signal: 9, SIGKILL", "Could not resolve host", "Connection timed out")):
            return 75
    print(f"Exact head {sha}: exit={result} {info['url']}")
    return result


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (subprocess.SubprocessError, OSError, ValueError, KeyError) as error:
        print(f"No verdict: {error}", file=sys.stderr)
        sys.exit(75)
