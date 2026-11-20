#!/usr/bin/env python3
"""Okilum merge queue: the one landing mechanism.

A pull request enters when its current head has a trusted REVIEW PASS or
CRITICAL-REVIEW PASS and a successful full ``agent-checks`` run (the
``linux`` job, executed on the build-slot pool). The queue then:

* fast-forwards ``main`` to that head when ``main`` is already an ancestor,
  because the green run is the exact tree that will land;
* otherwise merges ``main`` with one or more eligible pull requests, runs
  ``scripts/ci/pool-linux-gate.sh`` on that commit through ``build-slot``,
  and fast-forwards ``main`` only when the suite exits 0 and ``main`` is
  still the tested base.

A red suite ejects the last pull request in the batch and comments the
failing phase. Exit 75 (no verdict) retries the same batch next cycle. A
pull request that does not merge cleanly stays with the existing rebase
action. Forgejo has no native merge queue; this script is the queue.
"""
import argparse
import fcntl
import json
import os
import re
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request
from pathlib import Path

REPO = "BeFeast/okilum"
FORGEJO = "https://git.oklabs.uk"
GITHUB_REPO = "BeFeast/okilum"
TRUSTED = {"oklabs-bot", "oleg", "BeFeast"}
REVIEW_RX = re.compile(r"^(?:CRITICAL-)?REVIEW\s+(PASS|FIX)\s+([0-9a-f]{7,40})\b", re.I)
BATCH_CAP = 4
STATE = Path(os.path.expanduser("~/.local/state/okilum-merge-queue/state.json"))
LOCK = Path(os.path.expanduser("~/.local/state/okilum-merge-queue/lock"))
ENABLED = Path(os.path.expanduser("~/.local/state/okilum-merge-queue/enabled"))


def review_verdict(comments, head):
    """Latest trusted review line that names this head. -> (PASS|FIX|None, when)."""
    found = None
    ordered = sorted(comments or [], key=lambda item: item.get("created_at") or "")
    for comment in ordered:
        user = ((comment.get("user") or {}).get("login") or "")
        if user not in TRUSTED:
            continue
        lines = (comment.get("body") or "").splitlines()
        if not lines:
            continue
        match = REVIEW_RX.match(lines[0].strip())
        if not match:
            continue
        token = match.group(2).lower()
        if len(token) < 7 or not head.lower().startswith(token):
            continue
        found = (match.group(1).upper(), comment.get("created_at") or "")
    return found or (None, None)


def full_green(runs):
    """True when the newest full agent-checks run of this commit succeeded.

    A fast or benchmark ref is not the full gate. A newer failure wins over
    an older success.
    """
    full = []
    for run in runs or []:
        branch = run.get("headBranch") or run.get("branch") or ""
        if not branch.startswith("agent-checks/"):
            continue
        if branch.startswith(("agent-checks-fast/", "agent-checks-benchmark/")):
            continue
        full.append(run)
    if not full:
        return False
    newest = max(full, key=lambda item: item.get("createdAt") or item.get("created") or "")
    return newest.get("status") == "completed" and newest.get("conclusion") == "success"


def eligible(prs, comments_by_number, runs_by_sha):
    """Open non-draft PRs with PASS on the current head and a green full gate.

    Critical-labelled PRs go first, then the oldest PASS.
    """
    chosen = []
    for pr in prs or []:
        if pr.get("draft") or not pr.get("head"):
            continue
        verdict, when = review_verdict(comments_by_number.get(pr["number"]) or [], pr["head"])
        if verdict != "PASS":
            continue
        if not full_green(runs_by_sha.get(pr["head"]) or []):
            continue
        chosen.append({**pr, "passed_at": when, "critical": "critical" in (pr.get("labels") or [])})
    chosen.sort(key=lambda item: (not item["critical"], item["passed_at"] or "", item["number"]))
    return chosen


def ejection_key(number, head, base):
    return {"number": number, "head": head, "base": base}


def is_ejected(state, number, head, base):
    return ejection_key(number, head, base) in (state.get("ejected") or [])


def classify_landing(ancestor, main, heads):
    """'ff' when the single head already contains main; otherwise 'suite'."""
    if len(heads) == 1 and ancestor(main, heads[0]):
        return "ff"
    return "suite"


def suite_outcome(code, batch):
    """What to do after the pool suite. batch is non-empty."""
    if code == 0:
        return ("ff", None)
    if code in (75, 124):
        return ("retry", None)
    return ("eject", batch[-1])


def failing_phase(receipt_text):
    try:
        phases = json.loads(receipt_text or "[]")
    except ValueError:
        return None
    for phase in phases:
        if int(phase.get("code") or 0) != 0:
            return phase.get("name"), int(phase["code"])
    return None


def eject_comment(pr, base, tip, phase, log_tail):
    why = "the suite exited non-zero"
    if phase:
        why = f"phase `{phase[0]}` exited {phase[1]}"
    tail = (log_tail or "").strip()
    extra = f"\n\n```\n{tail[-4000:]}\n```" if tail else ""
    return (
        f"Merge queue ejected PR #{pr['number']}.\n\n"
        f"The full Linux suite on `{tip}` (main `{base[:12]}` + head `{pr['head'][:12]}`) "
        f"failed: {why}. This head is not retried against this main. "
        f"Push a new head, or wait until main moves, to enter the queue again."
        f"{extra}"
    )


def load_state(path=STATE):
    try:
        return json.loads(path.read_text())
    except (OSError, ValueError):
        return {"ejected": []}


def save_state(state, path=STATE):
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(state, indent=2) + "\n")
    temporary.replace(path)


class Queue:
    """Live landing. Tests use the functions above; this class shells out."""

    def __init__(self, root, state_path=STATE):
        self.root = Path(root)
        self.state_path = Path(state_path)

    def run(self, *args, check=True, timeout=120):
        return subprocess.run(args, cwd=self.root, text=True, check=check,
                              stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=timeout)

    def git(self, *args, check=True, timeout=120):
        return self.run("git", *args, check=check, timeout=timeout).stdout.strip()

    def once(self):
        state = load_state(self.state_path)
        self.git("fetch", "--no-tags", "origin", "main", timeout=180)
        main = self.git("rev-parse", "origin/main")
        prs = self.open_prs()
        comments = {pr["number"]: self.comments(pr["number"]) for pr in prs}
        runs = {}
        for pr in prs:
            runs[pr["head"]] = self.github_runs(pr["head"])
        waiting = [pr for pr in eligible(prs, comments, runs)
                   if not is_ejected(state, pr["number"], pr["head"], main)]
        if not waiting:
            print("merge queue: nothing eligible")
            return 0
        batch, tip = self.build_batch(main, waiting)
        if not batch:
            print("merge queue: eligible PRs do not merge cleanly; rebase action owns them")
            return 0
        heads = [pr["head"] for pr in batch]
        action = classify_landing(self.is_ancestor, main, heads)
        landing = heads[0] if action == "ff" else tip
        print(f"merge queue: {[pr['number'] for pr in batch]} action={action} landing={landing[:12]} base={main[:12]}")
        if action == "suite":
            code, receipt, tail = self.suite(landing)
            outcome, ejected = suite_outcome(code, batch)
            print(f"merge queue: suite exit={code} outcome={outcome}")
            if outcome == "retry":
                return 0
            if outcome == "eject":
                phase = failing_phase(receipt)
                self.comment(ejected["number"], eject_comment(ejected, main, landing, phase, tail))
                state.setdefault("ejected", []).append(ejection_key(ejected["number"], ejected["head"], main))
                state["ejected"] = state["ejected"][-200:]
                save_state(state, self.state_path)
                return 0
        current = self.git("rev-parse", "origin/main")
        if current != main:
            print("merge queue: main moved while the suite ran; not fast-forwarding")
            return 0
        pushed = self.push(landing, main)
        if pushed != 0:
            print("merge queue: fast-forward refused; main changed")
            return 0
        print(f"merge queue: fast-forwarded main to {landing}")
        return 0

    def build_batch(self, main, waiting):
        batch, tip = [], main
        for pr in waiting:
            if len(batch) >= BATCH_CAP:
                break
            if not self.fetch_head(pr):
                continue
            merged = self.merge(tip, pr["head"], pr["number"])
            if not merged:
                if not batch:
                    continue
                break
            batch.append(pr)
            tip = merged
        return batch, tip

    def fetch_head(self, pr):
        ref = f"refs/queue/{pr['number']}"
        self.git("fetch", "--no-tags", "origin", f"pull/{pr['number']}/head:{ref}", timeout=180)
        got = self.git("rev-parse", ref)
        if got != pr["head"]:
            print(f"merge queue: PR #{pr['number']} head moved during fetch")
            return False
        return True

    def is_ancestor(self, base, tip):
        return self.run("git", "merge-base", "--is-ancestor", base, tip, check=False).returncode == 0

    def merge(self, base, head, number):
        # Already contained: the commit itself is the merge result.
        if self.is_ancestor(base, head):
            return head
        tree = self.run("git", "merge-tree", "--write-tree", base, head, check=False)
        if tree.returncode != 0:
            print(f"merge queue: PR #{number} does not merge cleanly onto {base[:12]}")
            return None
        tree_sha = next(line.strip() for line in tree.stdout.splitlines() if re.fullmatch(r"[0-9a-f]{40}", line.strip()))
        message = f"Merge queue: land #{number}\n\nBase {base}\nHead {head}\n"
        commit = self.git("commit-tree", tree_sha, "-p", base, "-p", head, "-m", message)
        return commit

    def suite(self, sha):
        ship = Path(tempfile.mkdtemp(prefix="okilum-merge-queue-"))
        receipt = ship / "phases.json"
        label = f"merge-queue-{sha[:12]}"
        command = [
            "build-slot", "run", "--project", "okilum", "--label", label, "--owner", "merge-queue",
            "--timeout-minutes", "40", "--mem", "7",
            "--source-git", str(self.root), "--source-rev", sha,
            "--require-host", "baldr",
            "--ship", str(ship), "--env", f"OKILUM_POOL_RECEIPT={receipt}",
            "--", "bash", "scripts/ci/pool-linux-gate.sh",
        ]
        proc = subprocess.run(command, cwd=self.root, text=True, stdout=subprocess.PIPE,
                              stderr=subprocess.STDOUT, timeout=50 * 60)
        text = receipt.read_text() if receipt.exists() else ""
        return proc.returncode, text, proc.stdout

    def push(self, sha, expected):
        proc = self.run("git", "push", "origin", f"{sha}:refs/heads/main",
                        f"--force-with-lease=refs/heads/main:{expected}", check=False, timeout=180)
        return proc.returncode

    def open_prs(self):
        payload = self.api("GET", f"/repos/{REPO}/pulls?state=open&limit=50")
        out = []
        for pr in payload or []:
            labels = [item.get("name") for item in pr.get("labels") or [] if isinstance(item, dict)]
            out.append({
                "number": pr["number"],
                "draft": bool(pr.get("draft")),
                "head": (pr.get("head") or {}).get("sha") or "",
                "ref": (pr.get("head") or {}).get("ref") or "",
                "labels": [name for name in labels if name],
            })
        return out

    def comments(self, number):
        return self.api("GET", f"/repos/{REPO}/issues/{number}/comments?limit=50") or []

    def comment(self, number, body):
        self.api("POST", f"/repos/{REPO}/issues/{number}/comments", {"body": body})

    def github_runs(self, sha):
        proc = subprocess.run(
            ["gh", "run", "list", "-R", GITHUB_REPO, "--commit", sha, "-w", "agent-checks.yml",
             "-L", "20", "--json", "databaseId,headBranch,status,conclusion,createdAt"],
            text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=60, check=False)
        if proc.returncode != 0:
            print(proc.stdout[-500:], file=sys.stderr)
            return []
        return json.loads(proc.stdout or "[]")

    def api(self, method, path, body=None):
        token = forgejo_token()
        data = json.dumps(body).encode() if body is not None else None
        request = urllib.request.Request(
            FORGEJO + "/api/v1" + path, data=data, method=method,
            headers={"Authorization": f"token {token}", "Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                raw = response.read().decode()
        except urllib.error.HTTPError as error:
            detail = error.read().decode(errors="replace")[:300]
            raise RuntimeError(f"Forgejo {method} {path} -> {error.code} {detail}") from error
        return json.loads(raw) if raw else None


def forgejo_token():
    path = Path(os.path.expanduser("~/.config/maestro/private/forgejo.env"))
    for line in path.read_text().splitlines():
        if line.startswith("FORGEJO_TOKEN="):
            return line.split("=", 1)[1].strip().strip('"').strip("'")
    raise RuntimeError(f"FORGEJO_TOKEN is missing from {path}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=["once"])
    parser.add_argument("--root", default=os.getcwd())
    args = parser.parse_args()
    LOCK.parent.mkdir(parents=True, exist_ok=True)
    handle = LOCK.open("a")
    try:
        fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        print("merge queue: another cycle holds the lock")
        return 0
    ENABLED.parent.mkdir(parents=True, exist_ok=True)
    ENABLED.write_text("okilum merge queue owns landing\n")
    return Queue(args.root).once()


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, subprocess.SubprocessError, RuntimeError, KeyError, ValueError) as error:
        print(f"merge queue: no action: {error}", file=sys.stderr)
        sys.exit(75)
