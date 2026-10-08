#!/usr/bin/env python3
"""Bridge a trusted Forgejo PR head to GitHub's unsigned native gate.

No Forgejo credentials reach GitHub. The caller's final `macos` job reports the
result using Forgejo's normal job status, without using the corporate M4.
"""
import argparse
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

REPOSITORY = "BeFeast/tessera"
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


def push_head(branch, sha, token):
    actual = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
    if actual != sha:
        raise ValueError("Checkout is not the requested PR head")
    with tempfile.TemporaryDirectory(prefix="tessera-github-") as directory:
        askpass = Path(directory) / "askpass"
        askpass.write_text('#!/usr/bin/env python3\nimport os,sys\n'
                           'print("x-access-token" if "Username" in sys.argv[1] '
                           'else os.environ["MIRROR_TOKEN"])\n')
        askpass.chmod(0o700)
        env = dict(os.environ, MIRROR_TOKEN=token, GIT_ASKPASS=str(askpass),
                   GIT_TERMINAL_PROMPT="0")
        try:
            # Unique ref: never replace another attempt or publish internal tags.
            subprocess.run(["git", "-c", "credential.helper=", "push",
                            f"https://github.com/{REPOSITORY}.git",
                            f"{sha}:refs/heads/{branch}"], env=env, check=True,
                           timeout=120, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        except (subprocess.CalledProcessError, subprocess.TimeoutExpired):
            raise Unavailable("Could not push the GitHub native-check ref") from None


def hosted_request(api, path, *, sleep=time.sleep):
    """Retry transient hosted-service reads without creating duplicate builds."""
    for attempt in range(3):
        try:
            return api.request(path)
        except Unavailable:
            if attempt == 2:
                raise
            sleep(5 * (2 ** attempt))


def wait_for_run(api, branch, sha, *, clock=time.monotonic, sleep=time.sleep,
                 queue_timeout=1800, run_timeout=2400, workflow=WORKFLOW,
                 job_name="macos", build_step=BUILD_STEP):
    query = urllib.parse.urlencode({"branch": branch, "head_sha": sha,
                                    "event": "push", "per_page": 100})
    queued_until = clock() + queue_timeout
    running_until = None
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
                native = [job for job in jobs if job["name"] == job_name]
                if len(native) != 1 or native[0]["conclusion"] != "success" or not any(
                    step["name"] == build_step and step["conclusion"] == "success"
                    for step in native[0]["steps"]
                ):
                    return "failure", f"GitHub did not execute the native gate: {url}"
                return "success", f"GitHub native gate passed: {url}"
            if run["status"] == "in_progress" and running_until is None:
                running_until = clock() + run_timeout
            if running_until is not None and clock() >= running_until:
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
                    sleep(20)
                    continue
            raise Unavailable(f"GitHub did not start the gate within {queue_timeout} seconds")
        sleep(20)


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
    parser.add_argument("command", choices=["run", "cleanup"])
    args = parser.parse_args()
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
