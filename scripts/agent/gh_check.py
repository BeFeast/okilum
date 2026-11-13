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
    "Set up job", "Record exact source and runner", "Reclaim unused hosted SDKs for Rust artifacts",
    "Install system libraries",
    "Verify pinned Rust toolchain", "Restore Rust downloads and target cache",
    "Fetch locked public dependencies", "Save Rust downloads and target cache",
    "Prepare isolated Python test dependencies",
    "Restore Windows core cache", "Preserve vendor patch line endings",
}


def run(*args, **kwargs):
    kwargs.setdefault("timeout", 120)
    return subprocess.run(args, text=True, check=True, **kwargs)


def output(*args):
    return run(*args, stdout=subprocess.PIPE).stdout.strip()


def gh_json(*args):
    return json.loads(output("gh", *args))


# The pre-merge gate (Oleg 13.11 ~14:20): checks that compile nothing. The
# full set (this lane's ordinary verdict) runs once a day before Stable.
FAST_REQUIRED = {"Prepare and verify pinned vendor and comrak", "Check formatting",
                 "Verify agent helper controls", "Run CI, releases and updater Python suites",
                 "Verify bundled Sync Help and brand"}


def fast_verdict(info):
    """A pre-merge run counts only as itself: no other job may have run."""
    jobs = info.get("jobs", [])
    names = {job.get("name") for job in jobs}
    if (len(names) != len(jobs) or "pre-merge" not in names
            or not names <= {"pre-merge", "linux", "windows-recovery", "macos-sync"}):
        return 75
    gate = next(job for job in jobs if job.get("name") == "pre-merge")
    others = [job for job in jobs if job.get("name") != "pre-merge"]
    if info.get("conclusion") == "success":
        passed = {s["name"] for s in gate.get("steps", []) if s.get("conclusion") == "success"}
        return 0 if (FAST_REQUIRED <= passed and gate.get("conclusion") == "success"
                     and all(job.get("conclusion") == "skipped" for job in others)) else 75
    if info.get("conclusion") != "failure":
        return 75
    failed = [s for s in gate.get("steps", []) if s.get("conclusion") == "failure"]
    if not failed or any(s["name"] in INFRA_STEPS or "actions/checkout@" in s["name"] for s in failed):
        return 75
    return 1


def verdict(info, sha, benchmark=False, publication_controls=False, macos_sync=False, fast=False):
    """Missing/mismatched/partial/cancelled work never counts as a verdict."""
    if info.get("headSha") != sha or info.get("status") != "completed":
        return 75
    if fast:
        return fast_verdict(info)
    jobs = info.get("jobs", [])
    names = {job.get("name") for job in jobs}
    if (len(names) != len(jobs) or not {"linux", "windows-recovery"} <= names
            or not names <= {"linux", "windows-recovery", "macos-sync", "pre-merge"}
            or any(job.get("name") == "pre-merge" and job.get("conclusion") != "skipped" for job in jobs)
            or (macos_sync and "macos-sync" not in names)):
        return 75
    mac = next((job for job in jobs if job.get("name") == "macos-sync"), None)
    mac_steps = {"Prepare and verify Mac Sync vendor", "Lint native Mac Sync targets", "Run native Mac Sync controls"}
    linux = next(job for job in jobs if job.get("name") == "linux")
    windows = next(job for job in jobs if job.get("name") == "windows-recovery")
    steps = linux.get("steps", [])
    required = {"Prepare and verify pinned vendor and comrak", "Lint every workspace target offline",
                "Build Linux desktop offline", "Run complete okilum-core suite offline", "Run Linux Rust suites offline",
                "Verify agent helper controls", "Stress private reveal bus (20 runs)",
                "Run CI, releases and updater Python suites", "Verify per-user AppImage runtime",
                "Verify pinned Sync fixture lifecycle, source sharing and remote inspection",
                "Verify managed launch and configuration admission",
                "Reject and reap surviving agent fixture daemons"}
    if not benchmark:
        required.add("Verify additive vault regression controls")
    if publication_controls:
        required.add("Compare editor publication with pinned main on this host")
    if benchmark:
        required |= {"Warm build Linux desktop offline", "Warm complete okilum-core suite offline",
                     "Warm Linux Rust suites offline"}
    if info.get("conclusion") == "success":
        passed = {s["name"] for s in steps if s.get("conclusion") == "success"}
        native = {s["name"] for s in windows.get("steps", []) if s.get("conclusion") == "success"}
        if macos_sync:
            if (mac is None or mac.get("conclusion") != "success" or not mac_steps <= {
                    step["name"] for step in mac.get("steps", []) if step.get("conclusion") == "success"}):
                return 75
        elif mac is not None and mac.get("conclusion") not in ("skipped", "success"):
            return 75
        return 0 if (required <= passed and linux.get("conclusion") == "success"
                     and windows.get("conclusion") == "success"
                     and {"Prepare and verify Windows vendor", "Run native Windows recovery controls",
                          "Run native Windows delta console controls", "Run native Windows Sync identity controls",
                          "Run native Windows atlas upload controls"} <= native) else 75
    if info.get("conclusion") != "failure":
        return 75
    failed = [s for job in jobs for s in job.get("steps", []) if s.get("conclusion") == "failure"]
    if not failed or any(s["name"] in INFRA_STEPS or "actions/checkout@" in s["name"] for s in failed):
        return 75
    return 1



def infrastructure_failure(logs):
    # A watchdog's deliberate KILL is a failing test deadline, not an OOM/no
    # verdict. Explicit disk/memory/network failures still retain infra status.
    if any(marker in logs for marker in (
            "No space left on device", "Cannot allocate memory",
            "Could not resolve host", "Connection timed out")):
        return True
    return "TEST_GROUP_TIMEOUT group=" not in logs and any(
        marker in logs for marker in ("exit code 137", "signal: 9, SIGKILL"))


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
    parser.add_argument("--macos-sync", action="store_true", help="require native Mac Sync controls; requires --dispatch")
    parser.add_argument("--dispatch", action="store_true", help="dispatch after pushing instead of using the push run")
    parser.add_argument("--timeout", type=int, default=7200, help="total wait ceiling in seconds")
    parser.add_argument("--receipt", type=Path, help="also write the final GitHub JSON receipt to this path")
    parser.add_argument("--benchmark", action="store_true", help="repeat checks with warm target on the same runner VM")
    parser.add_argument("--fast", action="store_true",
                        help="pre-merge gate: formatting, Python and script checks; compiles nothing")
    parser.add_argument("--publication-baseline", help="public main SHA for same-host editor controls; requires --dispatch")
    args = parser.parse_args()
    if args.publication_baseline and (not args.dispatch or not re.fullmatch(r"[0-9a-f]{40}", args.publication_baseline)):
        parser.error("--publication-baseline requires --dispatch and a full lowercase SHA")
    if args.macos_sync and not args.dispatch:
        parser.error("--macos-sync requires --dispatch")
    if args.rerun and args.dispatch:
        parser.error("choose --rerun or --dispatch")
    if args.fast and (args.benchmark or args.macos_sync or args.publication_baseline):
        parser.error("--fast is the pre-merge gate; full-only controls need a full check")
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
    prefix = "agent-checks-fast/" if args.fast else "agent-checks/"
    branch = (f"agent-checks-benchmark/{sha}/" if args.benchmark else prefix) + branch
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
        if args.macos_sync:
            dispatch += ["-f", "macos_sync_check=true"]
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
    result = verdict(info, sha, args.benchmark, bool(args.publication_baseline), args.macos_sync, args.fast)
    if result:
        logs = subprocess.run(["gh", "run", "view", run_id, "-R", REPO, "--log-failed"],
                              text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=120)
        tail = logs.stdout.splitlines()[-100:]
        print("\nFailed-step log tail:\n" + "\n".join(tail))
        if logs.returncode:
            return 75
        # Shell exits for OOM/killed processes are infrastructure, not a code verdict.
        if infrastructure_failure(logs.stdout):
            return 75
    print(f"Exact head {sha}: exit={result} {info['url']}")
    return result


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (subprocess.SubprocessError, OSError, ValueError, KeyError) as error:
        print(f"No verdict: {error}", file=sys.stderr)
        sys.exit(75)
