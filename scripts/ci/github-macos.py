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


def gate_execution(env, checkout):
    """Only this trusted manual CI execution may report for a distinct product SHA."""
    if (env.get('GITHUB_REPOSITORY') != REPOSITORY or env.get('GITHUB_REF') != 'refs/heads/main'
            or env.get('GITHUB_EVENT_NAME') != 'workflow_dispatch'
            or env.get('GITHUB_RUN_ATTEMPT') != '1' or env.get('GITHUB_JOB') != 'reconcile'
            or env.get('GITHUB_SHA') != checkout or not re.fullmatch('[0-9a-f]{40}', checkout)):
        raise ValueError('Only first-attempt trusted main reconciliation CI may report')
    for name in ['GITHUB_RUN_ID', 'GITHUB_RUN_NUMBER']:
        if not re.fullmatch('[1-9][0-9]*', env.get(name, '')):
            raise ValueError('Missing reconciliation execution identity')
    return {'tooling': checkout, 'id': int(env['GITHUB_RUN_ID']),
            'number': int(env['GITHUB_RUN_NUMBER'])}


def prepare_gates(forgejo, github, execution, *, sha, number, forgejo_run, attempt,
                  run_id, job_id, evidence):
    """Re-evaluate the original required gates; no compiler or status mutation."""
    # Native verifier creates the exclusive root and retains its exact API inputs.
    proof = reconcile(github, sha=sha, number=number, forgejo_run=forgejo_run,
                      attempt=attempt, run_id=run_id, job_id=job_id, evidence=evidence)
    root = Path(evidence)
    def read(path, name):
        data = forgejo.call('GET', path)
        (root / name).write_text(json.dumps(data, indent=2) + '\n')
        return data
    current = read(f'/pulls/{number}', 'pr.json')
    validate_pr(current, sha)
    main = read('/branches/main', 'main.json')
    if main.get('commit', {}).get('id') != execution['tooling']:
        raise ValueError('Trusted main changed; no reconciliation report')
    tooling_ci = read(f"/commits/{execution['tooling']}/status", 'tooling-ci.json')
    for context in ['ci / linux (push)', 'ci / check (push)']:
        matches = [s for s in tooling_ci.get('statuses', []) if s.get('context') == context]
        if len(matches) != 1 or matches[0].get('status') != 'success':
            raise ValueError('Reviewed main tooling has not passed its normal CI gates')
    own = read(f"/actions/runs/{execution['id']}", 'execution.json')
    if (own.get('id') != execution['id'] or own.get('index_in_repo') != execution['number']
            or own.get('workflow_id') != 'ci-reconcile.yml' or own.get('event') != 'workflow_dispatch'
            or own.get('commit_sha') != execution['tooling']
            or own.get('repository', {}).get('full_name') != REPOSITORY
            or own.get('status') not in ['running', 'waiting']):
        raise ValueError('Reconciliation execution is not the trusted workflow/source')
    own_inputs = json.loads(own.get('event_payload', '{}')).get('inputs', {})
    expected_inputs = {'source': sha, 'pr': str(number), 'forgejo_run': str(forgejo_run),
                       'github_run': str(run_id), 'github_job': str(job_id)}
    if own_inputs != expected_inputs:
        raise ValueError('Execution dispatch inputs do not bind this reconciliation')
    protection = read('/branch_protections/main', 'required-contexts.json')
    if (protection.get('enable_status_check') is not True
            or set(protection.get('status_check_contexts', [])) != {
                'ci / macos (pull_request)', 'ci / check (pull_request)'}):
        raise ValueError('Required branch-gate policy changed; explicit review needed')
    own_jobs = read(f"/actions/runs/{execution['id']}/jobs", 'execution-jobs.json')
    if (not isinstance(own_jobs, list) or len(own_jobs) != 1
            or own_jobs[0].get('name') != 'reconcile'
            or type(own_jobs[0].get('attempt')) is not int or own_jobs[0]['attempt'] != 1
            or own_jobs[0].get('run_id') != execution['id']
            or own_jobs[0].get('status') != 'running'):
        raise ValueError('Reconciliation is not one first-attempt trusted CI job')
    original = read(f'/actions/runs/{forgejo_run}', 'original-run.json')
    event = json.loads(original.get('event_payload', '{}'))
    if (original.get('id') != int(forgejo_run) or original.get('workflow_id') != 'ci.yml'
            or original.get('commit_sha') != sha or original.get('event') != 'pull_request'
            or original.get('repository', {}).get('full_name') != REPOSITORY
            or original.get('status') != 'failure'
            or type(original.get('index_in_repo')) is not int
            or original.get('is_fork_pull_request') is not False
            or event.get('number') != int(number)):
        raise ValueError('Original CI run does not belong to this trusted PR/source')
    validate_pr(event.get('pull_request', {}), sha, require_open=False)
    jobs = read(f'/actions/runs/{forgejo_run}/jobs', 'original-jobs.json')
    if not isinstance(jobs, list):
        raise ValueError('Missing original jobs')
    # This is the existing hosted code-PR branch of ci.yml, not docs/local fallback.
    expected = {'native-scope': 'success', 'linux-local': 'skipped',
                'linux-github': 'success', 'main-health': 'skipped', 'linux': 'success',
                'macos-github': 'success', 'macos-local': 'skipped',
                'windows-github': 'success', 'perf-linux': 'success',
                'macos': 'failure', 'check': 'failure'}
    if len(jobs) != len(expected) or {j.get('name') for j in jobs} != set(expected):
        raise ValueError('Changed, missing or ambiguous original CI gate schema')
    for j in jobs:
        if (j.get('run_id') != int(forgejo_run)
                or type(j.get('attempt')) is not int or j['attempt'] != 1
                or j.get('status') != expected[j['name']]):
            raise ValueError('Another required job failed, reran or is incomplete')
    statuses = read(f'/commits/{sha}/status', 'original-statuses.json')
    by_context = {s['context']: s for s in statuses.get('statuses', [])}
    if len(by_context) != len(statuses.get('statuses', [])):
        raise ValueError('Ambiguous latest commit statuses')
    for index, j in enumerate(jobs):
        context = f"ci / {j['name']} (pull_request)"
        observed = by_context.get(context, {})
        target = f"/BeFeast/okilum/actions/runs/{original['index_in_repo']}/jobs/{index}"
        if (observed.get('status') != expected[j['name']]
                or observed.get('target_url') not in [target, 'https://git.oklabs.uk' + target]):
            raise ValueError('Latest required status is not the exact original CI execution')
    proof.update({'classification': 'EXACT_REQUIRED_GATES_REEVALUATED', 'execution': execution,
                  'original_index': original['index_in_repo'], 'original_failure_retained': True,
                  'report_contexts': ['ci / macos (pull_request)', 'ci / check (pull_request)']})
    (root / 'gate-receipt.json').write_text(json.dumps(proof, indent=2) + '\n')
    return proof


def validate_pr(pr, sha, require_open=True):
    if (pr.get('head', {}).get('sha') != sha
            or pr.get('head', {}).get('repo', {}).get('full_name') != REPOSITORY
            or pr.get('base', {}).get('ref') != 'main' or pr.get('draft') is not False
            or (require_open and (pr.get('state') != 'open' or pr.get('merged') is not False))):
        raise ValueError('PR source/repository/base/draft/state changed')


def report_gates(forgejo, proof, *, evidence_uploaded):
    """Automatic CI reporting only after fresh full verification and evidence upload."""
    if evidence_uploaded != 'success' or proof.get('classification') != 'EXACT_REQUIRED_GATES_REEVALUATED':
        raise ValueError('No uploaded verified gate evidence')
    execution = proof['execution']
    if proof['report_contexts'] != ['ci / macos (pull_request)', 'ci / check (pull_request)']:
        raise ValueError('Unexpected reporting contexts')
    artifacts = forgejo.call('GET', f"/actions/runs/{execution['id']}/artifacts")
    expected_name = f"ci-reconcile-{execution['id']}-{proof['source']}"
    matches = [a for a in artifacts.get('artifacts', []) if a.get('name') == expected_name]
    if (len(matches) != 1 or not isinstance(matches[0].get('id'), int)
            or matches[0]['id'] <= 0 or matches[0].get('expired') is not False
            or matches[0].get('size_in_bytes', 0) <= 0):
        raise ValueError('Required evidence artifact not retained for this execution/source')
    target = f"https://git.oklabs.uk/BeFeast/okilum/actions/runs/{execution['number']}"
    # Source may advance while metadata is read. A new head refuses reporting.
    # Status API appends records: the original failed jobs/statuses remain history.
    try:
        for context in proof['report_contexts']:
            validate_pr(forgejo.call('GET', f"/pulls/{proof['pr']}"), proof['source'])
            forgejo.call('POST', f"/statuses/{proof['source']}", {
                'state': 'success', 'context': context, 'target_url': target,
                'description': f"Verified native reuse; CI {execution['number']}; original {proof['original_index']} retained"})
        current = forgejo.call('GET', f"/commits/{proof['source']}/status")
        for context in proof['report_contexts']:
            statuses = [s for s in current.get('statuses', []) if s.get('context') == context]
            if len(statuses) != 1 or statuses[0].get('status') != 'success' or statuses[0].get('target_url') != target:
                raise ValueError('Reported status readback unavailable/inconsistent; no retry')
    except Exception:
        # Revoke only this execution's own derived successes if reporting is partial.
        # Never overwrite a newer result from another execution, reset old jobs,
        # retry a status POST, or claim rollback when the service is unavailable.
        try:
            current = forgejo.call('GET', f"/commits/{proof['source']}/status")
            for context in proof['report_contexts']:
                own = [s for s in current.get('statuses', [])
                       if s.get('context') == context and s.get('target_url') == target
                       and s.get('status') == 'success']
                if len(own) == 1:
                    forgejo.call('POST', f"/statuses/{proof['source']}", {
                        'state': 'error', 'context': context, 'target_url': target,
                        'description': 'Reconciliation reporting incomplete; STOP, no retry'})
        except Exception:
            print('Reconciliation reporting/rollback UNCONFIRMED; STOP, no retry')
        raise
    return {'classification': 'RECONCILED_REQUIRED_GATES_REPORTED', 'target': target,
            'source': proof['source'], 'tooling': execution['tooling'], 'status_writes': 2,
            'compiler_dispatches': 0, 'original_failure_retained': True}


def reconcile_gates_main(args):
    # Reuse the repository's existing authenticated Forgejo status/issue API client.
    import importlib.util
    spec = importlib.util.spec_from_file_location('main_health_api', Path(__file__).with_name('main-health.py'))
    health = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(health)
    checkout = subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip()
    execution = gate_execution(os.environ, checkout)
    forgejo = health.Api('https://git.oklabs.uk', REPOSITORY, os.environ['FORGEJO_TOKEN'])
    github = GitHub(os.environ['MIRROR_TOKEN'])
    kwargs = dict(sha=args.source, number=args.pr, forgejo_run=args.forgejo_run,
                  attempt=args.attempt, run_id=args.github_run, job_id=args.github_job)
    proof = prepare_gates(forgejo, github, execution, evidence=args.evidence, **kwargs)
    if args.phase == 'report':
        # Only the trusted workflow supplies this literal previous-step outcome.
        result = report_gates(forgejo, proof, evidence_uploaded=os.environ.get('EVIDENCE_UPLOADED'))
        (Path(args.evidence)/'report.json').write_text(json.dumps(result, indent=2)+'\n')
        print(json.dumps(result))
    else:
        print(json.dumps(proof))


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
    parser.add_argument("command", choices=["run", "cleanup", "reconcile", "reconcile-gates"])
    for name in ("source", "pr", "forgejo-run", "attempt", "github-run", "github-job", "evidence"):
        parser.add_argument("--" + name)
    parser.add_argument("--phase", choices=["verify", "report"], default="verify")
    args = parser.parse_args()
    if args.command in ["reconcile", "reconcile-gates"]:
        required = (args.source, args.pr, args.forgejo_run, args.attempt,
                    args.github_run, args.github_job, args.evidence)
        if not all(required):
            parser.error("reconcile requires all exact identities and a NEW evidence directory")
        if args.command == "reconcile-gates":
            reconcile_gates_main(args)
            return
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
