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
import importlib.util
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


def gh_check():
    """The same verdict the merge rule already uses for a full agent-checks run."""
    path = Path(__file__).resolve().parents[1] / "agent" / "gh_check.py"
    spec = importlib.util.spec_from_file_location("okilum_gh_check", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def newest_full_run(runs):
    """Newest agent-checks run that is not a fast or benchmark ref, or None."""
    full = []
    for run in runs or []:
        branch = run.get("headBranch") or run.get("branch") or ""
        if not branch.startswith("agent-checks/"):
            continue
        if branch.startswith(("agent-checks-fast/", "agent-checks-benchmark/")):
            continue
        full.append(run)
    if not full:
        return None
    return max(full, key=lambda item: item.get("createdAt") or item.get("created") or "")


def runs_after_view(listed, view):
    """Load the newest full run before any older one.

    ``view(run)`` returns the run view, or None when that read fails. A failed
    read of the newest run blocks landing; an older success must not replace it.
    """
    newest = newest_full_run(listed)
    if newest is None:
        return {"unreadable": False, "runs": []}
    detail = view(newest)
    if detail is None:
        return {"unreadable": True, "runs": []}
    return {"unreadable": False, "runs": [detail]}


def full_green(runs, sha):
    """True when the newest full agent-checks run passes gh_check.verdict.

    A workflow conclusion of success is not enough: a skipped linux job, or a
    run missing a required step, must not land. Fast and benchmark refs are
    not the full gate. A newer failure wins over an older success.
    """
    newest = newest_full_run(runs)
    if newest is None or not sha:
        return False
    return gh_check().verdict(newest, sha) == 0


def gate_ok(record, sha):
    """False when the newest run could not be read, or its verdict is not green."""
    if isinstance(record, dict):
        if record.get("unreadable"):
            return False
        record = record.get("runs") or []
    return full_green(record, sha)


def eligible(prs, comments_by_number, runs_by_sha):
    """Open non-draft PRs with PASS on the current head and a green full gate.

    Critical-labelled PRs go first, then the oldest PASS.
    """
    chosen = []
    for pr in prs or []:
        if pr.get("draft") or not pr.get("head") or pr.get("base") != "main":
            continue
        verdict, when = review_verdict(comments_by_number.get(pr["number"]) or [], pr["head"])
        if verdict != "PASS":
            continue
        if not gate_ok(runs_by_sha.get(pr["head"]) or [], pr["head"]):
            continue
        chosen.append({**pr, "passed_at": when, "critical": "critical" in (pr.get("labels") or [])})
    chosen.sort(key=lambda item: (not item["critical"], item["passed_at"] or "", item["number"]))
    return chosen


def ejection_key(number, head, base):
    return {"number": number, "head": head, "base": base}


def is_ejected(state, number, head, base):
    return ejection_key(number, head, base) in (state.get("ejected") or [])


def still_landable(batch, prs, comments_by_number, runs_by_sha):
    """False when any selected PR's head, base, review or full gate moved."""
    current = {pr["number"]: pr for pr in prs or []}
    for item in batch:
        pr = current.get(item["number"])
        if not pr or pr.get("draft") or pr.get("base") != "main" or pr.get("head") != item["head"]:
            return False
        verdict, _when = review_verdict(comments_by_number.get(item["number"]) or [], item["head"])
        if verdict != "PASS":
            return False
        if not gate_ok(runs_by_sha.get(item["head"]) or [], item["head"]):
            return False
    return True


def pages(fetch, limit=50, max_pages=40):
    """Follow Forgejo list pages until a short page. fetch(page) -> list."""
    out = []
    for page in range(1, max_pages + 1):
        batch = fetch(page) or []
        if not isinstance(batch, list):
            break
        out.extend(batch)
        if len(batch) < limit:
            break
    return out


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
        fresh = self.open_prs()
        fresh_comments = {pr["number"]: self.comments(pr["number"]) for pr in batch}
        fresh_runs = {pr["head"]: self.github_runs(pr["head"]) for pr in batch}
        if not still_landable(batch, fresh, fresh_comments, fresh_runs):
            print("merge queue: review, head or gate changed; not fast-forwarding")
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
        root = Path("/home/god/scratch/okilum-pool-receipts")
        root.mkdir(parents=True, exist_ok=True)
        ship = Path(tempfile.mkdtemp(prefix="merge-queue-", dir=root))
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
        payload = pages(lambda page: self.api(
            "GET", f"/repos/{REPO}/pulls?state=open&limit=50&page={page}"))
        out = []
        for pr in payload:
            labels = [item.get("name") for item in pr.get("labels") or [] if isinstance(item, dict)]
            out.append({
                "number": pr["number"],
                "draft": bool(pr.get("draft")),
                "head": (pr.get("head") or {}).get("sha") or "",
                "ref": (pr.get("head") or {}).get("ref") or "",
                "base": (pr.get("base") or {}).get("ref") or "",
                "labels": [name for name in labels if name],
            })
        return out

    def comments(self, number):
        return pages(lambda page: self.api(
            "GET", f"/repos/{REPO}/issues/{number}/comments?limit=50&page={page}"))

    def comment(self, number, body):
        self.api("POST", f"/repos/{REPO}/issues/{number}/comments", {"body": body})

    def github_runs(self, sha):
        proc = subprocess.run(
            ["gh", "run", "list", "-R", GITHUB_REPO, "--commit", sha, "-w", "agent-checks.yml",
             "-L", "20", "--json", "databaseId,headBranch,status,conclusion,createdAt"],
            text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=60, check=False)
        if proc.returncode != 0:
            print(proc.stdout[-500:], file=sys.stderr)
            return {"unreadable": True, "runs": []}
        listed = json.loads(proc.stdout or "[]")
        fields = "headSha,headBranch,status,conclusion,jobs,url,attempt,createdAt"

        def view(run):
            seen = subprocess.run(
                ["gh", "run", "view", str(run["databaseId"]), "-R", GITHUB_REPO, "--json", fields],
                text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=60, check=False)
            if seen.returncode != 0:
                print(seen.stdout[-500:], file=sys.stderr)
                return None
            return json.loads(seen.stdout or "{}")

        return runs_after_view(listed, view)

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
