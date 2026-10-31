#!/usr/bin/env python3
"""Report a red main to the people who made it red, and say when it is green again.

    main-health.py red|green

Main CI cancels superseded runs, so the failing commit may not be the culprit:
every merge since the last green main is a suspect. Green means a Linux lane
passed: a cancelled run leaves its aggregate `check` failed or pending forever,
so that status says nothing. Each suspect PR gets one
comment per red episode, addressed to its author, and a single "main is red"
issue carries the current state for the manager. A green main closes that issue.

A failure that never reached the build or tests (runner, Docker, network, disk) is
reported as likely infrastructure and blames no merge. Its error markers count only on
the runner's own lines, never in test or script output. A job that hit its time limit
with a test still running hung in that test, which is code. Forgejo has no API to re-run a job,
so a human re-runs it or the next merge retests. For real failures the failing
tests are listed. A lane log that cannot be read proves nothing: the merges are blamed
and the issue says why the log was not read.

Environment: FORGEJO_URL, REPOSITORY, FORGEJO_TOKEN, SHA, RUN_URL, RUN_ID, and an optional
DRY_RUN=1 that prints instead of writing.
"""
import json
import os
import re
import subprocess
import sys
import urllib.parse
import urllib.request

LANES = tuple(f'ci / {lane} ({event})'
              for event in ('push', 'schedule', 'workflow_dispatch')
              for lane in ('linux-local', 'linux-github'))
ISSUE_TITLE = 'main is red'
RULE = ('Rule: whoever merged the commit that turned main red reverts it or lands a fix '
        'within 20 minutes (AGENTS.md, "Red main").')
MAX_SUSPECTS = 20
AWAITING = 'Awaiting confirmation'
RED_SHA = re.compile(r'failed on main at ([0-9a-f]{7,40})')
PR_SUBJECT = re.compile(r"^Merge pull request '.*' \(#(\d+)\) from ")


class Api:
    def __init__(self, url, repository, token, dry_run=False):
        self.base = f"{url.rstrip('/')}/api/v1/repos/{repository}"
        self.token = token
        self.dry_run = dry_run

    def text(self, path):
        request = urllib.request.Request(self.base + path, headers={'Authorization': f'token {self.token}'})
        with urllib.request.urlopen(request, timeout=60) as response:
            return response.read().decode('utf-8', 'replace')

    def call(self, method, path, body=None):
        if self.dry_run and method != 'GET':
            print(f'DRY {method} {path} {json.dumps(body)[:400]}')
            return {}
        data = None if body is None else json.dumps(body).encode()
        request = urllib.request.Request(self.base + path, data=data, method=method, headers={
            'Authorization': f'token {self.token}', 'Content-Type': 'application/json'})
        with urllib.request.urlopen(request, timeout=30) as response:
            text = response.read()
        return json.loads(text) if text else {}

    def green(self, sha):
        statuses = self.call('GET', f'/commits/{sha}/statuses?limit=50')
        return lane_passed(statuses)

    def open_issue(self):
        query = urllib.parse.urlencode({'state': 'open', 'type': 'issues', 'q': ISSUE_TITLE, 'limit': 50})
        return next((i for i in self.call('GET', f'/issues?{query}') if i['title'] == ISSUE_TITLE), None)


# The runner's words when the job's timeout-minutes run out, not a runner fault (#1962).
DEADLINE = 'context deadline exceeded'
# Counted only on the runner's own lines (#1962): tests and scripts print the same words,
# and a passing test_github_pr.py prints 'Hosted CI unavailable' in nearly every linux-local run.
# 'failed to create container' goes before 'Error response from daemon': act's cleanup error
# names the daemon first and the cause in "(original error: ...)".
INFRA_MARKERS = ('Cannot connect to the Docker daemon', 'failed to create container',
                 'Error response from daemon', 'No space left on device', 'Could not resolve host',
                 'Connection reset by peer', DEADLINE)
# The runner's step and job errors, and act's cleanup error: the only line that carries a
# Docker failure when the job container was never created. The runner always writes the gear;
# a bare "[runner]: " is a step echoing one.
RUNNER_LINE = re.compile(r'^⚙️ \[runner\]: .*$|^Error occurred running finally: .*$', re.M)
FAILED_TEST = re.compile(r'^---- (\S+) stdout ----$', re.M)
SLOW_TEST = re.compile(r'^test (\S+) has been running for over 60 seconds$', re.M)
LOG_PREFIX = re.compile(r'^\S+Z ', re.M)
# unittest's header for a failed or errored test: 'FAIL: test_x (module.Class.test_x)'. Python
# before 3.11 names only the class in the parentheses; a subTest appends its parameters.
PYTHON_FAILURE = re.compile(r'^(?:FAIL|ERROR): (\S+) \(([^()\s]+)\)', re.M)
PYTHON_FAILED = re.compile(r'^FAILED \(', re.M)


def python_failures(text):
    """Full identifiers of the tests a failed unittest run reported, once each, in log order."""
    names = []
    for match in PYTHON_FAILURE.finditer(text):
        method, where = match.groups()
        name = where if where.endswith(f'.{method}') else f'{where}.{method}'
        if name not in names:
            names.append(name)
    return names


def unfinished_tests(text):
    """Tests that cargo reported as slow and that never printed a result."""
    names = []
    for slow in SLOW_TEST.finditer(text):
        name = slow.group(1)
        result = re.compile(rf'^test {re.escape(name)} \.\.\. ', re.M)
        if name not in names and not result.search(text, slow.end()):
            names.append(name)
    return names


def classify(log):
    """('code', failing tests), ('infra', reason) or ('unknown', []) for a failed lane log."""
    if log is None:  # not read: only a log that was read can clear the merges
        return 'unknown', []
    text = LOG_PREFIX.sub('', log)
    tests = FAILED_TEST.findall(text)
    if tests or 'test result: FAILED' in text:
        return 'code', tests
    if re.search(r'^error(\[E\d+\])?: ', text, re.M):  # a compiler error
        return 'code', []
    # A Python unittest suite that failed. Its headers count only with that summary: a passing
    # suite's output may print the same shape.
    if PYTHON_FAILED.search(text):
        return 'code', python_failures(text)
    for line in RUNNER_LINE.findall(text):
        for marker in INFRA_MARKERS:
            if marker in line:
                # Out of time with a test still running: the test hung (run 10704). When every
                # slow test finished, the job was still making progress and the host was slow.
                hung = unfinished_tests(text) if marker == DEADLINE else []
                return ('code', hung) if hung else ('infra', marker)
    if not re.search(r'^\s*(Compiling|Checking|Finished|Running) ', text, re.M):
        return 'infra', 'the job ended before any build or test output'
    return 'unknown', []


def lane_log(api, run_id):
    """The failed Linux lane's log in this run; raises when it cannot be read."""
    jobs = api.call('GET', f'/actions/runs/{run_id}/jobs')
    jobs = jobs if isinstance(jobs, list) else jobs.get('jobs', [])
    job = next((j for j in jobs if j['name'] == 'linux-local' and j['status'] == 'failure'), None)
    if job is None:
        raise LookupError(f'no failed linux-local job in run {run_id}')
    return api.text(f"/actions/jobs/{job['id']}/logs")


def error_line(error):
    """The exception as one short line that is safe inside a Markdown code span."""
    # str(URLError) is '<urlopen error ...>', which Markdown would take for an HTML tag.
    return ' '.join(f'{type(error).__name__}: {error}'.split())[:300].replace('`', "'")


def lane_passed(statuses):
    # Newest first: the first status for each context is the current one.
    current = {}
    for status in statuses:
        current.setdefault(status.get('context'), status.get('status'))
    return any(current.get(lane) == 'success' for lane in LANES)


def first_parents(sha, limit):
    out = subprocess.run(['git', 'rev-list', '--first-parent', f'--max-count={limit}', '--format=%H %s', sha],
                         check=True, capture_output=True, text=True).stdout
    commits = []
    for line in out.splitlines():
        if line.startswith('commit '):
            continue
        commit, _, subject = line.partition(' ')
        commits.append((commit, subject))
    return commits


def is_ancestor(old, new):
    return subprocess.run(['git', 'merge-base', '--is-ancestor', old, new], capture_output=True).returncode == 0


def tip():
    # The job checks out main as it is now, which may be ahead of the commit tested.
    return subprocess.run(['git', 'rev-parse', 'HEAD'], check=True, capture_output=True, text=True).stdout.strip()


def pull_number(subject):
    match = PR_SUBJECT.match(subject)
    return int(match.group(1)) if match else None


def suspects(api, sha):
    """Main commits from `sha` back to the last green one (exclusive), and that green commit."""
    found = []
    for commit, subject in first_parents(sha, MAX_SUSPECTS + 1):
        if commit != sha and api.green(commit):
            return found, commit
        found.append((commit, subject))
    return found[:MAX_SUSPECTS], None


def superseded(api, sha):
    """A newer main commit that contains `sha` already passed: this red is history."""
    head = tip()
    if head == sha or not is_ancestor(sha, head):
        return False
    for commit, _ in first_parents(head, MAX_SUSPECTS):
        if commit == sha:
            return False
        if api.green(commit):
            return True
    return False


def red(api, sha, run_url, run_id=None):
    # Each main commit runs on its own, so runs can finish out of order.
    if superseded(api, sha):
        print(f'{sha[:8]} is red, but a newer main commit containing it is green')
        return
    log = unread = None
    if run_id:
        try:
            log = lane_log(api, run_id)
        except Exception as error:  # a missing permission or log must not hide the red main
            unread = error_line(error)
            print(f'Cannot read the lane log: {unread}')
    kind, detail = classify(log)
    if kind == 'infra':
        body = (f'The Linux gate failed on main at {sha[:8]}: {run_url}\n\n'
                f'**This looks like infrastructure, not a merge**: {detail}. No merge is blamed. '
                'Re-run the job in the web UI (Forgejo has no API for it) or let the next merge retest; '
                'if it fails again with build or test output, the merges are reported.\n'
                'This issue closes itself when main is green again.')
        issue = api.open_issue()
        if issue is None:
            api.call('POST', '/issues', {'title': ISSUE_TITLE, 'body': body})
        else:
            api.call('POST', f"/issues/{issue['number']}/comments", {'body': body})
        return
    failing = (''.join(f'\n- `{t}`' for t in detail) + '\n\n') if detail else ''
    # One failing test that the next commit may pass is how this lane's flakes look
    # (#1179): the first such red opens the issue without waking anyone; a second red main
    # commit in the same episode confirms it. Compile errors and unknown reds blame at once.
    issue = api.open_issue()
    if kind == 'code' and len(detail) == 1 and (issue is None or not reported_red(api, issue)):
        body = (f'The Linux gate failed on main at {sha[:8]}: {run_url}\n\nFailing tests:{failing}'
                f'**{AWAITING}**: a single failing test, which the next main commit usually passes. '
                'No merge is blamed yet; a second red main commit blames the merges since the last green one.\n'
                'This issue closes itself when main is green again.')
        if issue is None:
            api.call('POST', '/issues', {'title': ISSUE_TITLE, 'body': body})
        else:
            api.call('POST', f"/issues/{issue['number']}/comments", {'body': body})
        return
    commits, green = suspects(api, sha)
    episode = green or 'unknown'
    marker = f'<!-- main-red {episode[:12]} -->'
    lines = []
    for commit, subject in commits:
        number = pull_number(subject)
        if number is None:
            lines.append(f'- {commit[:8]} {subject} (direct push)')
            continue
        pull = api.call('GET', f'/pulls/{number}')
        author = pull.get('user', {}).get('login', '?')
        lines.append(f'- #{number} by @{author} ({commit[:8]})')
        comments = api.call('GET', f'/issues/{number}/comments?limit=50')
        if any(marker in c.get('body', '') for c in comments):
            continue
        api.call('POST', f'/issues/{number}/comments', {'body': (
            f'{marker}\n@{author} main is red at {sha[:8]} ({run_url}). This merge landed after the '
            f'last green main ({episode[:8]}), so it is a suspect.\n\n{RULE}')})
    body = (f'The Linux gate failed on main at {sha[:8]}: {run_url}\n\n'
            + (f'Lane log unreadable: `{unread}`; classified as unknown.\n\n' if unread else '')
            + (f'Failing tests:{failing}' if failing else '') +
            f'Last green main: {episode[:8]}. Merges since then:\n' + '\n'.join(lines) + f'\n\n{RULE}\n'
            'This issue closes itself when main is green again.')
    issue = api.open_issue()
    if issue is None:
        api.call('POST', '/issues', {'title': ISSUE_TITLE, 'body': body})
    else:
        api.call('POST', f"/issues/{issue['number']}/comments", {'body': body})


def reported_red(api, issue):
    reports = [issue.get('body', '')] + [c.get('body', '') for c in
                                         api.call('GET', f"/issues/{issue['number']}/comments?limit=50")]
    found = [m.group(1) for m in (RED_SHA.search(r) for r in reports) if m]
    return found[-1] if found else None


def green(api, sha, run_url):
    issue = api.open_issue()
    if issue is None:
        return
    red_sha = reported_red(api, issue)
    # An older commit finishing green late says nothing about the newer red one.
    if red_sha and not is_ancestor(red_sha, sha):
        print(f'{sha[:8]} is green but does not contain the reported red {red_sha}')
        return
    api.call('POST', f"/issues/{issue['number']}/comments", {'body': f'main is green again at {sha[:8]}: {run_url}'})
    api.call('PATCH', f"/issues/{issue['number']}", {'state': 'closed'})


def main():
    state = sys.argv[1]
    env = os.environ
    api = Api(env['FORGEJO_URL'], env['REPOSITORY'], env['FORGEJO_TOKEN'], env.get('DRY_RUN') == '1')
    if state == 'red':
        red(api, env['SHA'], env['RUN_URL'], env.get('RUN_ID'))
    else:
        green(api, env['SHA'], env['RUN_URL'])


if __name__ == '__main__':
    main()
