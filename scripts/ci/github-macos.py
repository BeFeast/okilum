#!/usr/bin/env python3
"""Bridge a trusted Forgejo PR head to GitHub's unsigned native gate.

No Forgejo credentials reach GitHub. The caller's final `macos` job reports the
result using Forgejo's normal job status, without using the corporate M4.
"""
import argparse
import http.client
import importlib.util
import random
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


spec = importlib.util.spec_from_file_location('github_bridge_auth', Path(__file__).with_name('github_auth.py'))
auth = importlib.util.module_from_spec(spec)
spec.loader.exec_module(auth)


class RateLimitExhausted(Unavailable):
    pass


def poll_delay():
    return random.uniform(30, 60)


class GitHub:
    def __init__(self, token=None):
        self.quota_exhausted = False
        try:
            self.credentials = auth.Credentials(fallback=token) if token is None or isinstance(token, str) else token
        except auth.CredentialError as error:
            raise Unavailable(str(error)) from None

    @property
    def token(self):
        try:
            return self.credentials.token
        except auth.RateLimitError as error:
            self.quota_exhausted = True
            raise RateLimitExhausted(str(error)) from None
        except auth.CredentialError as error:
            raise Unavailable(str(error)) from None

    def log_quota(self):
        try:
            self.credentials.log_quota()
        except auth.RateLimitError as error:
            self.quota_exhausted = True
            raise RateLimitExhausted(str(error)) from None
        except auth.CredentialError as error:
            raise Unavailable(str(error)) from None

    def request(self, path, method="GET", data=None):
        if self.quota_exhausted:
            raise RateLimitExhausted('GitHub API quota exhausted earlier in this job; no further request')
        request_path = path
        headers = {"Authorization": f"Bearer {self.token}",
                   "Accept": "application/vnd.github+json",
                   "X-GitHub-Api-Version": "2022-11-28"}
        if method == "GET" and path.startswith("actions/"):
            separator = "&" if "?" in path else "?"
            request_path += f"{separator}_bridge_read={uuid.uuid4().hex}"
            headers["Cache-Control"] = "no-cache"
        request = urllib.request.Request(
            f"https://api.github.com/repos/{REPOSITORY}/{request_path}",
            data=json.dumps(data).encode() if data is not None else None,
            method=method, headers=headers)
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                self.credentials.note_quota(getattr(response, 'headers', {}))
                body = response.read()
                return json.loads(body) if body else None
        except urllib.error.HTTPError as error:
            self.credentials.note_quota(error.headers)
            reason = auth.http_error(error, f"GitHub {method} {path.split('?')[0]}")
            if isinstance(reason, auth.RateLimitError):
                self.quota_exhausted = True
                raise RateLimitExhausted(str(reason)) from None
            raise Unavailable(str(reason)) from None
        except (urllib.error.URLError, TimeoutError,
                http.client.RemoteDisconnected, ConnectionResetError) as error:
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


DIAGNOSTIC_LIMIT = 4096


def safe_diagnostic(message, token):
    """Redact before bounding so truncation cannot expose a credential prefix."""
    message = str(message)
    if token:
        for secret in (token, urllib.parse.quote(token, safe="")):
            message = message.replace(secret, "[REDACTED]")
    message = re.sub(r"https?://[^\s/@]+(?::[^\s/@]*)?@", "https://[REDACTED]@", message)
    message = re.sub(r"(?i)(authorization[:=]\s*(?:bearer|basic)\s+)\S+",
                     r"\1[REDACTED]", message)
    message = re.sub(r"[\x00-\x08\x0b-\x1f\x7f]", "?", message)
    return message[:DIAGNOSTIC_LIMIT] + (" [truncated]" if len(message) > DIAGNOSTIC_LIMIT else "")


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
                reason = safe_diagnostic("\n".join(lines), token) if lines else f"git exit {error.returncode}"
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
        except RateLimitExhausted:
            raise
        except Unavailable:
            if attempt == 2:
                raise
            sleep(poll_delay())


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
                # The completed run and its jobs/steps are separate API projections.
                # Re-read this exact run's evidence before rejecting a green result;
                # do not send it through the execution deadline/cancellation path.
                for attempt in range(3):
                    jobs = []
                    for page in range(1, 11):
                        path = f"actions/runs/{run['id']}/jobs?per_page=100"
                        if page > 1:
                            path += f"&page={page}"
                        batch = hosted_request(api, path, sleep=sleep)["jobs"]
                        jobs.extend(batch)
                        if len(batch) < 100:
                            break
                    else:
                        raise Unavailable(f"GitHub jobs pagination exceeded 10 pages: {url}")
                    native = [job for job in jobs if job["name"] == job_name]
                    steps = [step for job in native for step in job.get("steps", [])
                             if step.get("name") == build_step]
                    if (len(native) == 1 and native[0].get("conclusion") == "success"
                            and len(steps) == 1 and steps[0].get("conclusion") == "success"):
                        return "success", f"GitHub native gate passed: {url}"
                    evidence = [{key: job.get(key) for key in
                                 ("id", "name", "status", "conclusion", "steps")}
                                for job in jobs]
                    log(f"GitHub gate evidence {attempt + 1}/3: run_id={run['id']} "
                        f"status={run['status']} conclusion={run['conclusion']} "
                        f"expected_job={job_name!r} expected_step={build_step!r} "
                        f"jobs={evidence!r} — {url}", flush=True)
                    if attempt < 2:
                        sleep(poll_delay())
                # A persistently missing, skipped or failed gate must still fail.
                return "failure", f"GitHub did not execute the native gate: {url}"
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
                    sleep(poll_delay())
                    continue
            raise Unavailable(f"GitHub did not start the gate within {queue_timeout} seconds")
        sleep(poll_delay())


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
    if args.command == "cleanup":
        api = GitHub()
        api.log_quota()
        cleanup(api, os.environ["PR_NUMBER"])
        return
    result = "failure"
    try:
        api = GitHub()
        sha = os.environ["PR_HEAD_SHA"]
        branch = ref_name(os.environ["PR_NUMBER"], sha, os.environ["GITHUB_RUN_ID"],
                          (os.environ.get("GITHUB_RUN_ATTEMPT") or "1"), uuid.uuid4().hex)
        api.log_quota()
        push_head(branch, sha, api.token)
        result, message = wait_for_run(api, branch, sha)
        print(message)
    except Unavailable as error:
        result = "unavailable"
        print(f"GitHub macOS unavailable — rerun later: {error}")
    finally:
        with open(os.environ["GITHUB_OUTPUT"], "a") as output:
            output.write(f"result={result}\n")


if __name__ == "__main__":
    main()
