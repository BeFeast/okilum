#!/usr/bin/env python3
"""Bridge a trusted Forgejo PR head to GitHub's unsigned native gate.

No Forgejo credentials reach GitHub. The caller's final `macos` job reports the
result using Forgejo's normal job status, without using the corporate M4.
"""
import argparse
from datetime import datetime
import http.client
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

REPOSITORY = "BeFeast/okilum"
WORKFLOW = ".github/workflows/forgejo-macos.yml"
BUILD_STEP = "Compile Reader and run quick native regressions"


class Unavailable(Exception):
    """The remote service could not provide a native result."""


class GitHub:
    def __init__(self, token):
        self.token = token

    def request(self, path, method="GET", data=None):
        request = urllib.request.Request(
            f"https://api.github.com/repos/{REPOSITORY}/{path}",
            data=json.dumps(data).encode() if data is not None else None,
            method=method,
            headers={"Authorization": f"Bearer {self.token}",
                     "Accept": "application/vnd.github+json",
                     "X-GitHub-Api-Version": "2022-11-28"},
        )
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                body = response.read()
                return json.loads(body) if body else None
        except (urllib.error.URLError, TimeoutError,
                http.client.RemoteDisconnected, ConnectionResetError) as error:
            # Avoid printing HTTP response bodies or credential-bearing commands.
            raise Unavailable(f"GitHub {method} {path.split('?')[0]}: {type(error).__name__}") from None


def ref_name(number, sha, run_id, attempt, invocation=None):
    if not re.fullmatch(r"[0-9a-f]{40}", sha):
        raise ValueError("Expected a full PR head SHA")
    if any(not re.fullmatch(r"[1-9][0-9]*", str(value))
           for value in (number, run_id, attempt)):
        raise ValueError("Expected positive PR/run/attempt identifiers")
    if invocation is not None and not re.fullmatch(r"[0-9a-f]{32}", invocation):
        raise ValueError("Expected a UUID hex invocation identifier")
    suffix = f"-{invocation}" if invocation is not None else ""
    return f"forgejo-pr/{number}/{sha}-{run_id}-{attempt}{suffix}"


PUSH_ATTEMPTS = 3
PUSH_RETRY_SECONDS = 15


def push_head(branch, sha, token, *, sleep=time.sleep):
    actual = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
    if actual != sha:
        raise ValueError("Checkout is not the requested PR head")
    with tempfile.TemporaryDirectory(prefix="okilum-github-") as directory:
        askpass = Path(directory) / "askpass"
        askpass.write_text('#!/usr/bin/env python3\nimport os,sys\n'
                           'print("x-access-token" if "Username" in sys.argv[1] '
                           'else os.environ["MIRROR_TOKEN"])\n')
        askpass.chmod(0o700)
        env = dict(os.environ, MIRROR_TOKEN=token, GIT_ASKPASS=str(askpass),
                   GIT_TERMINAL_PROMPT="0")
        # GitHub sometimes rejects a push transiently ("remote rejected ... (failed)"):
        # retry before reporting the lane unavailable, and say what git said. The token
        # reaches git through askpass, never on the command line or in its output.
        reason = ""
        for attempt in range(PUSH_ATTEMPTS):
            try:
                # Unique ref: never replace another attempt or publish internal tags.
                subprocess.run(["git", "-c", "credential.helper=", "push",
                                f"https://github.com/{REPOSITORY}.git",
                                f"{sha}:refs/heads/{branch}"], env=env, check=True,
                               timeout=120, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                return
            except subprocess.CalledProcessError as error:
                lines = (error.stderr or b"").decode("utf-8", "replace").strip().splitlines()
                reason = lines[-1] if lines else f"git exit {error.returncode}"
            except subprocess.TimeoutExpired:
                reason = "git push timed out"
            if attempt + 1 < PUSH_ATTEMPTS:
                sleep(PUSH_RETRY_SECONDS * (attempt + 1))
        raise Unavailable(f"Could not push the GitHub native-check ref: {reason}")


def hosted_request(api, path, *, sleep=time.sleep):
    """Retry transient hosted-service reads without creating duplicate builds."""
    for attempt in range(3):
        try:
            return api.request(path)
        except Unavailable:
            if attempt == 2:
                raise
            sleep(5 * (2 ** attempt))


# GitHub's hosted macOS pool is small and queues for well over an hour when
# several PRs land at once; waiting is not a failure (2026-10-09). The
# Forgejo job timeout in ci.yml must exceed QUEUE_TIMEOUT + RUN_TIMEOUT.
QUEUE_TIMEOUT = 3 * 3600
RUN_TIMEOUT = 2400
PROGRESS_EVERY = 300


def wait_for_run(api, branch, sha, *, clock=time.monotonic, sleep=time.sleep,
                 queue_timeout=QUEUE_TIMEOUT, run_timeout=RUN_TIMEOUT, workflow=WORKFLOW,
                 job_name="macos", build_step=BUILD_STEP, log=print):
    query = urllib.parse.urlencode({"branch": branch, "head_sha": sha,
                                    "event": "push", "per_page": 100})
    started = clock()
    queued_until = started + queue_timeout
    running_until = None
    never_started = False
    next_progress = started + PROGRESS_EVERY
    while True:
        runs = hosted_request(api, f"actions/runs?{query}", sleep=sleep)["workflow_runs"]
        matches = [run for run in runs if run["head_sha"] == sha
                   and run["head_branch"] == branch and run["event"] == "push"
                   and run["path"] == workflow]
        if len(matches) > 1:
            return "failure", "Ambiguous GitHub workflow runs"
        if matches:
            run = matches[0]
            url = run["html_url"]
            if run["status"] == "completed":
                if run["conclusion"] == "startup_failure":
                    raise Unavailable(f"GitHub runner could not start: {url}")
                if run["conclusion"] != "success":
                    return "failure", f"GitHub native gate: {run['conclusion']} — {url}"
                jobs = hosted_request(api, f"actions/runs/{run['id']}/jobs?per_page=100", sleep=sleep)["jobs"]
                # A green workflow with a skipped/removed native job is not evidence.
                if not native_gate_executed(jobs, job_name, build_step):
                    return "failure", f"GitHub did not execute the native gate: {url}"
                return "success", f"GitHub native gate passed: {url}"
            if run["status"] == "in_progress":
                never_started = False
                if running_until is None:
                    running_until = clock() + run_timeout
        if running_until is None and clock() >= next_progress:
            next_progress = clock() + PROGRESS_EVERY
            where = f": {matches[0]['html_url']}" if matches else ""
            log(f"Waiting for a GitHub macOS runner, queued {int(clock() - started) // 60} min{where}", flush=True)
        if running_until is not None and clock() >= running_until:
            if never_started:
                return "failure", (f"GitHub macOS runner did not start within {queue_timeout // 60} min "
                                   f"and the queued run could not be cancelled: {url}")
            # Do not turn a hanging test into a successful retry on another host.
            try:
                api.request(f"actions/runs/{run['id']}/cancel", "POST")
            except Unavailable:
                pass  # A cancellation outage cannot turn a hung test into fallback.
            return "failure", f"GitHub native gate exceeded its execution deadline: {url}"
        if running_until is None and clock() >= queued_until:
            if matches:
                try:
                    api.request(f"actions/runs/{matches[0]['id']}/cancel", "POST")
                except Unavailable:
                    # Cancellation may race with runner startup or be refused.
                    # Keep observing this exact run within one execution budget;
                    # never turn a live run into an unavailable/fallback result.
                    running_until = clock() + run_timeout
                    never_started = matches[0]["status"] == "queued"
                    sleep(20)
                    continue
            raise Unavailable(f"GitHub did not start the gate within {queue_timeout} seconds")
        sleep(20)



def native_gate_executed(jobs, job_name="macos", build_step=BUILD_STEP):
    """The original required predicate, shared by polling and reconciliation."""
    native = [job for job in jobs if job.get("name") == job_name]
    return (len(native) == 1 and native[0].get("conclusion") == "success"
            and any(step.get("name") == build_step and step.get("conclusion") == "success"
                    for step in native[0].get("steps", [])))


def reconcile(api, *, sha, number, forgejo_run, attempt, run_id, job_id, evidence):
    """Read an existing exact native result. Never write gate outputs or statuses.

    An observation cannot retroactively repair the original Forgejo aggregate.
    The exclusive receipt retains the API inputs even when validation refuses.
    """
    ref_name(number, sha, forgejo_run, attempt)
    if str(attempt) != "1" or any(not re.fullmatch(r"[1-9][0-9]*", str(n))
                                for n in (run_id, job_id)):
        raise ValueError("Only exact first-attempt run/job identities are accepted")
    evidence = Path(evidence)
    evidence.mkdir(mode=0o700, parents=False, exist_ok=False)
    def read(path, name):
        # No polling/retry/path variants: retain the exact current API response.
        value = api.request(path)
        (evidence / name).write_text(json.dumps(value, indent=2) + "\n")
        return value
    run = read(f"actions/runs/{run_id}", "run.json")
    jobs = read(f"actions/runs/{run_id}/attempts/1/jobs?per_page=100", "jobs.json")
    workflow = read("actions/workflows/forgejo-macos.yml", "workflow.json")
    expected = ref_name(number, sha, forgejo_run, attempt)
    branch = run.get("head_branch", "")
    if not re.fullmatch(re.escape(expected) + r"-[0-9a-f]{32}", branch):
        raise ValueError("Run does not belong to the exact original Forgejo invocation")
    if (type(run.get("id")) is not int or run["id"] != int(run_id)
            or type(run.get("run_attempt")) is not int or run["run_attempt"] != 1 or run.get("head_sha") != sha
            or run.get("event") != "push" or run.get("path") != WORKFLOW
            or run.get("status") != "completed" or run.get("conclusion") != "success"
            or run.get("repository", {}).get("full_name") != REPOSITORY
            or run.get("head_repository", {}).get("full_name") != REPOSITORY
            or run.get("html_url") != f"https://github.com/{REPOSITORY}/actions/runs/{run_id}"
            or workflow.get("path") != WORKFLOW or workflow.get("state") != "active"
            or type(workflow.get("id")) is not int
            or run.get("workflow_id") != workflow["id"]):
        raise ValueError("Missing or inconsistent run/source/workflow identity")
    items = jobs.get("jobs", [])
    if type(jobs.get("total_count")) is not int or jobs["total_count"] != 1 or len(items) != 1 or not native_gate_executed(items):
        raise ValueError("Missing, ambiguous or unexecuted native job")
    job = items[0]
    steps = [s for s in job.get("steps", []) if s.get("name") == BUILD_STEP]
    if (type(job.get("id")) is not int or job["id"] != int(job_id)
            or type(job.get("run_id")) is not int or job["run_id"] != int(run_id)
            or type(job.get("run_attempt")) is not int or job["run_attempt"] != 1 or job.get("head_sha") != sha
            or job.get("status") != "completed" or len(steps) != 1
            or steps[0].get("status") != "completed"
            or not steps[0].get("started_at") or not steps[0].get("completed_at")):
        raise ValueError("Missing or inconsistent exact job/attempt/executed step")
    def timestamp(value):
        if not isinstance(value, str) or not value.endswith("Z"):
            raise ValueError("Missing or malformed execution timestamp")
        return datetime.fromisoformat(value.replace("Z", "+00:00"))
    if not (timestamp(job.get("started_at")) <= timestamp(steps[0]["started_at"])
            < timestamp(steps[0]["completed_at"]) <= timestamp(job.get("completed_at"))
            <= timestamp(run.get("updated_at"))):
        raise ValueError("Inconsistent native execution chronology")
    result = {"classification": "CURRENT_EXACT_NATIVE_SUCCESS",
              "source": sha, "pr": int(number), "forgejo_run": int(forgejo_run),
              "github_run": int(run_id), "github_job": int(job_id), "attempt": 1,
              "branch": branch, "workflow": WORKFLOW,
              "historical_mismatch_cause": "UNKNOWN",
              "required_forgejo_aggregate": "UNCHANGED",
              "compiler_dispatches": 0, "status_writes": 0}
    return result


def cleanup(api, number):
    if not re.fullmatch(r"[1-9][0-9]*", str(number)):
        raise ValueError("Expected a positive PR number")
    prefix = f"refs/heads/forgejo-pr/{number}/"
    refs = api.request(f"git/matching-refs/heads/forgejo-pr/{number}/")
    for ref in refs:
        if ref["ref"].startswith(prefix):
            api.request("git/" + ref["ref"], "DELETE")
            print(f"Deleted {ref['ref']}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=["run", "cleanup", "reconcile"])
    for name in ("source", "pr", "forgejo-run", "attempt", "github-run", "github-job", "evidence"):
        parser.add_argument("--" + name)
    args = parser.parse_args()
    if args.command == "reconcile":
        required = (args.source, args.pr, args.forgejo_run, args.attempt,
                    args.github_run, args.github_job, args.evidence)
        if not all(required):
            parser.error("reconcile requires all exact identities and a NEW evidence directory")
        token = os.environ.get("MIRROR_TOKEN", "")
        if not token:
            raise Unavailable("GitHub mirror token is unavailable")
        result = reconcile(GitHub(token), sha=args.source, number=args.pr,
                           forgejo_run=args.forgejo_run, attempt=args.attempt,
                           run_id=args.github_run, job_id=args.github_job, evidence=args.evidence)
        (Path(args.evidence) / "receipt.json").write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps(result))
        return
    token = os.environ.get("MIRROR_TOKEN", "")
    if args.command == "cleanup":
        if not token:
            raise Unavailable("GitHub mirror token is unavailable")
        cleanup(GitHub(token), os.environ["PR_NUMBER"])
        return
    result = "failure"
    try:
        if not token:
            raise Unavailable("GitHub mirror token is unavailable")
        sha = os.environ["PR_HEAD_SHA"]
        branch = ref_name(os.environ["PR_NUMBER"], sha, os.environ["GITHUB_RUN_ID"],
                          (os.environ.get("GITHUB_RUN_ATTEMPT") or "1"), uuid.uuid4().hex)
        push_head(branch, sha, token)
        result, message = wait_for_run(GitHub(token), branch, sha)
        print(message)
    except Unavailable as error:
        result = "unavailable"
        print(f"GitHub macOS unavailable — rerun later: {error}")
    finally:
        with open(os.environ["GITHUB_OUTPUT"], "a") as output:
            output.write(f"result={result}\n")


if __name__ == "__main__":
    main()
