#!/usr/bin/env python3
"""Report a red main to the people who made it red, and say when it is green again.

    main-health.py red|green

Main CI cancels superseded runs, so the failing commit may not be the culprit:
every merge since the last green main is a suspect. Green means a Linux lane
passed: a cancelled run leaves its aggregate `check` failed or pending forever,
so that status says nothing. Each suspect PR gets one
comment per red episode, addressed to its author, and a single "main is red"
issue carries the current state for the manager. A green main closes that issue.

Environment: FORGEJO_URL, REPOSITORY, FORGEJO_TOKEN, SHA, RUN_URL, and an optional
DRY_RUN=1 that prints instead of writing.
"""
import json
import os
import re
import subprocess
import sys
import urllib.parse
import urllib.request

LANES = ('ci / linux-local (push)', 'ci / linux-github (push)')
ISSUE_TITLE = 'main is red'
RULE = ('Rule: whoever merged the commit that turned main red reverts it or lands a fix '
        'within 20 minutes (AGENTS.md, "Red main").')
MAX_SUSPECTS = 20
RED_SHA = re.compile(r'failed on main at ([0-9a-f]{7,40})')
PR_SUBJECT = re.compile(r"^Merge pull request '.*' \(#(\d+)\) from ")


class Api:
    def __init__(self, url, repository, token, dry_run=False):
        self.base = f"{url.rstrip('/')}/api/v1/repos/{repository}"
        self.token = token
        self.dry_run = dry_run

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


def red(api, sha, run_url):
    # Each main commit runs on its own, so runs can finish out of order.
    if superseded(api, sha):
        print(f'{sha[:8]} is red, but a newer main commit containing it is green')
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
    {'red': red, 'green': green}[state](api, env['SHA'], env['RUN_URL'])


if __name__ == '__main__':
    main()
