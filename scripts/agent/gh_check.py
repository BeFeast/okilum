#!/usr/bin/env python3
"""Push committed HEAD to the public mirror and return an exact-source verdict."""
import argparse
import json
import subprocess
import sys
import time
from datetime import datetime, timezone

REPO = "BeFeast/okilum"
WORKFLOW = "agent-checks.yml"
INFRA_STEPS = {
    "Set up job", "Record exact source and runner", "Install system libraries",
    "Verify pinned Rust toolchain", "Restore Rust downloads and target cache",
    "Fetch locked public dependencies", "Save Rust downloads and target cache",
}


def run(*args, **kwargs):
    return subprocess.run(args, text=True, check=True, **kwargs)


def output(*args):
    return run(*args, stdout=subprocess.PIPE).stdout.strip()


def gh_json(*args):
    return json.loads(output("gh", *args))


def verdict(info, sha):
    """Missing/mismatched/partial/cancelled work never counts as a verdict."""
    if info.get("headSha") != sha or info.get("status") != "completed":
        return 75
    jobs = info.get("jobs", [])
    if len(jobs) != 1 or jobs[0].get("name") != "linux":
        return 75
    steps = jobs[0].get("steps", [])
    required = {"Prepare and verify pinned vendor and comrak",
                "Run complete okilum-core suite offline", "Run full Linux gate offline"}
    if info.get("conclusion") == "success":
        passed = {s["name"] for s in steps if s.get("conclusion") == "success"}
        return 0 if required <= passed and jobs[0].get("conclusion") == "success" else 75
    if info.get("conclusion") != "failure":
        return 75
    failed = [s for s in steps if s.get("conclusion") == "failure"]
    if not failed or any(s["name"] in INFRA_STEPS or s["name"].startswith("actions/checkout") for s in failed):
        return 75
    return 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rerun", action="store_true", help="rerun this exact SHA (also measures warm cache)")
    parser.add_argument("--dispatch", action="store_true", help="dispatch after pushing instead of using the push run")
    parser.add_argument("--timeout", type=int, default=7200, help="total wait ceiling in seconds")
    args = parser.parse_args()
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
    branch = "agent-checks/" + branch
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
    dispatched_at = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    if args.dispatch:
        run("gh", "workflow", "run", WORKFLOW, "-R", REPO, "--ref", branch)
    found = None
    # GitHub's run index is eventually consistent. Polling lives only inside
    # this single bounded helper process, never in an agent/thread loop.
    while time.monotonic() < deadline:
        runs = gh_json("run", "list", "-R", REPO, "--workflow", WORKFLOW,
                       "--branch", branch, "--commit", sha, "--event", event,
                       "--limit", "30", "--json", "databaseId,headSha,status,createdAt")
        matches = [r for r in runs if r["headSha"] == sha and
                   (not args.dispatch or r["createdAt"] >= dispatched_at)]
        if matches:
            found = matches[0]
            break
        time.sleep(min(5, max(0, deadline - time.monotonic())))
    if not found:
        print("No exact-head workflow run appeared before the deadline.", file=sys.stderr)
        return 75
    run_id = str(found["databaseId"])
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
    run("gh", "run", "watch", run_id, "-R", REPO, "--compact", "--interval", "15",
        timeout=max(1, deadline - time.monotonic()))
    info = gh_json("run", "view", run_id, "-R", REPO, "--json",
                   "headSha,status,conclusion,jobs,url,attempt,createdAt,updatedAt")
    print(json.dumps(info, indent=2))
    if attempt is not None and info["attempt"] <= attempt:
        return 75
    result = verdict(info, sha)
    if result:
        logs = subprocess.run(["gh", "run", "view", run_id, "-R", REPO, "--log-failed"],
                              text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        tail = logs.stdout.splitlines()[-100:]
        print("\nFailed-step log tail:\n" + "\n".join(tail))
        if logs.returncode:
            return 75
        # Shell exits for OOM/killed processes are infrastructure, not a code verdict.
        if any("exit code 137" in line or "No space left on device" in line or "Cannot allocate memory" in line for line in tail):
            return 75
    print(f"Exact head {sha}: exit={result} {info['url']}")
    return result


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (subprocess.SubprocessError, OSError, ValueError, KeyError) as error:
        print(f"No verdict: {error}", file=sys.stderr)
        sys.exit(75)
