#!/usr/bin/env bash
# Sign a Windows build with Oleg's current SimplySign code (#1104). Run on maestro:
#   okilum-sign-code SOURCE_SHA BUILD          the code is read from stdin, never echoed
#   echo 123456 | okilum-sign-code SOURCE_SHA BUILD
# A SimplySign code lives about 30 s and works once, while a CI job needs about a minute to
# reach its first signature. So the code is spent here, at once, on the signer (CT 142):
# ssign logs in by signing a non-production test PE (the dry test) and caches the session in
# /run/okilum-ssign for 20 minutes. Then windows-diagnostic re-signs BUILD of SOURCE, reusing
# that session, and the W2 check reads the published result. The code never reaches a log,
# a file, a command line or Forgejo. Promotion stays a separate, manual step.
set -Eeuo pipefail
SRC=${1:?source commit} BUILD=${2:?build to re-sign}
[[ "$SRC" =~ ^[0-9a-f]{40}$ && "$BUILD" =~ ^[0-9]+$ ]] || { echo "usage: okilum-sign-code SOURCE_SHA BUILD" >&2; exit 2; }
SIGNER=root@10.10.0.237
A=https://git.oklabs.uk/api/v1/repos/BeFeast/okilum CDN=https://updates.befeast.com/okilum
# Status lines (never the code) also go to a log, so whoever follows the run sees the outcome.
LOG="$HOME/.cache/okilum-sign-code.log"
say() { echo "[$(TZ=Asia/Jerusalem date '+%F %H:%M:%S')] $*" | tee -a "$LOG"; }
set -a; . "$HOME/.config/forgejo/token.env"; set +a
api() { curl -fsS -H "Authorization: token $FORGEJO_TOKEN" -H 'Content-Type: application/json' "$@"; }

lock="${XDG_RUNTIME_DIR:-/tmp}/okilum-sign-code.lock.d"
mkdir "$lock" 2>/dev/null || { say "another okilum-sign-code run holds $lock"; exit 1; }
trap 'rmdir "$lock"' EXIT

# This file is also the source of truth for the installed, standalone helper.
# Explicit operator approval only: JSON array of exact identities, default [].
# Inventory and every cancel decision finish BEFORE reading the one-use OTP.
python3 - <<'CANCEL_GUARD'
import json
import os
import re
import subprocess
import sys
import time

BASE = "https://git.oklabs.uk/api/v1/repos/BeFeast/okilum"
WORKFLOW = "windows-diagnostic.yml"
ACTIVE = {"waiting", "running", "blocked", "queued"}
FIELDS = {"id", "index_in_repo", "workflow_id", "ref", "commit_sha", "owner", "repo"}
deadline = time.monotonic() + 60


def stop(reason):
    raise ValueError(reason)


def request(path, post=False):
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        stop("inventory/cancellation deadline exceeded")
    # No retries: a timeout or partial response must never become an empty inventory.
    args = ["curl", "-fsS", "--connect-timeout", "5", "--max-time", "20",
            "-H", "Authorization: token " + os.environ["FORGEJO_TOKEN"]]
    if post:
        args += ["-X", "POST"]
    result = subprocess.run(args + [BASE + path], capture_output=True,
                            timeout=min(20, remaining), check=True)
    return None if post else json.loads(result.stdout)


def identity(run):
    if (type(run["id"]) is not int or run["id"] <= 0
            or type(run["index_in_repo"]) is not int or run["index_in_repo"] <= 0):
        stop("invalid run identity")
    repo = run["repository"]
    if (repo["full_name"] != "BeFeast/okilum" or repo["owner"]["login"] != "BeFeast"
            or repo["name"] != "okilum" or repo["fork"] is not False
            or run["is_fork_pull_request"] is not False
            or run["trigger_event"] != "schedule" or run["is_ref_deleted"] is not False):
        stop("foreign, fork, deleted ref or non-scheduled run")
    return dict(id=run["id"], index_in_repo=run["index_in_repo"],
                workflow_id=run["workflow_id"], ref=run["prettyref"],
                commit_sha=run["commit_sha"], owner=repo["owner"]["login"], repo=repo["name"])


def inventory():
    # Forgejo's nonterminal statuses, including unknown (which is rejected below).
    # Hard bound: one complete page of <=50. Larger inventories require disposition.
    data = request("/actions/runs?workflow_id=" + WORKFLOW +
                   "&status=unknown&status=waiting&status=running&status=blocked&status=queued&limit=50&page=1")
    runs = data["workflow_runs"]
    count = data["total_count"]
    if (not isinstance(runs, list) or type(count) is not int
            or count != len(runs) or not 0 <= count <= 50):
        stop("incomplete or oversized inventory")
    found = {}
    for run in runs:
        actual = identity(run)
        rid = actual["id"]
        if (run["status"] not in ACTIVE or actual["workflow_id"] != WORKFLOW
                or rid not in approved or actual != approved[rid] or rid in found):
            stop("new, unknown or mismatched run in inventory")
        found[rid] = actual
    return found


try:
    entries = json.loads(os.environ.get("OKILUM_SIGN_CANCEL_ALLOWLIST", "[]"))
    if not isinstance(entries, list) or len(entries) > 10:
        stop("allowlist must be an array of at most 10 identities")
    approved = {}
    for entry in entries:
        if (not isinstance(entry, dict) or set(entry) != FIELDS
                or type(entry["id"]) is not int or entry["id"] <= 0
                or type(entry["index_in_repo"]) is not int or entry["index_in_repo"] <= 0
                or entry["workflow_id"] != WORKFLOW or entry["ref"] != "refs/heads/main"
                or not isinstance(entry["commit_sha"], str)
                or not re.fullmatch("[0-9a-f]{40}", entry["commit_sha"])
                or entry["owner"] != "BeFeast" or entry["repo"] != "okilum"
                or entry["id"] in approved):
            stop("invalid or duplicate approved identity")
        approved[entry["id"]] = entry
    # Forgejo exposes prettyref as 'main'; approval uses an unambiguous full ref.
    for entry in approved.values():
        entry["ref"] = "main"
    if set(inventory()) != set(approved):
        stop("approved targets missing from initial inventory")
    for rid, expected in approved.items():
        current = inventory()
        if rid not in current:
            stop("target disappeared before cancellation")
        # Detail read is the last request immediately before the cancel POST.
        target = request("/actions/runs/" + str(rid))
        if identity(target) != expected or target["status"] not in ACTIVE:
            stop("target changed before cancellation")
        request("/actions/runs/" + str(rid) + "/cancel", post=True)
        print("Requested cancellation of explicitly approved run " + str(expected["index_in_repo"]), flush=True)
    if inventory():
        stop("cancellation not yet complete; OTP was not read")
except (ValueError, KeyError, TypeError, OSError, subprocess.SubprocessError) as error:
    # Do not echo API bodies, credentials or command arguments.
    reason = str(error) if type(error) is ValueError else type(error).__name__
    print("BLOCKER: cancellation guard stopped (" + reason + "); OTP was not read", file=sys.stderr)
    sys.exit(1)
CANCEL_GUARD

if [ -t 0 ]; then read -rsp 'SimplySign code: ' code; echo; else read -r code; fi
[[ "$code" =~ ^[0-9]{6}$ ]] || { say "the code must be 6 digits; nothing was sent"; exit 2; }
email=$(api "$A/actions/variables/OKILUM_CERTUM_EMAIL" | python3 -c 'import json,sys;print(json.load(sys.stdin)["data"])')
[[ "$email" =~ ^[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+$ ]] || { echo "OKILUM_CERTUM_EMAIL is not a plain e-mail address" >&2; exit 1; }

say "logging in on the signer with the code (dry-sign of the test PE)"
# The code and the e-mail travel on ssh's stdin into ssign's environment only; the remote
# command line is a fixed string with nothing interpolated into it.
if ! printf '%s\n%s\n' "$code" "$email" | ssh -o BatchMode=yes -i "$HOME/.ssh/id_ed25519_maestro" "$SIGNER" \
    "su okilum-signer -s /bin/bash -c 'set -e; read -r c; read -r e; out=\$(mktemp -d); exec 9>/run/lock/okilum-ssign.lock; flock -w 60 9
      CERTUM_EMAIL=\$e CERTUM_TOKEN=\$c XDG_RUNTIME_DIR=/run/okilum-ssign timeout 120 /usr/local/bin/ssign -o \"\$out\" /usr/local/share/okilum-signer/test.exe >/dev/null
      test -f /run/okilum-ssign/ssign/session.json; rm -rf \"\$out\"'"; then
    unset code
    say "BLOCKER: login with the code failed (expired or mistyped?); nothing was dispatched"
    exit 1
fi
unset code
say "session open on the signer for 20 minutes; dry-sign of the test PE succeeded"

since=$(date -u +%Y-%m-%dT%H:%M:%S)
api -o /dev/null -X POST -d "{\"ref\":\"main\",\"inputs\":{\"source\":\"$SRC\",\"resign_build\":\"$BUILD\"}}" \
    "$A/actions/workflows/windows-diagnostic.yml/dispatches"
say "dispatched re-sign of $BUILD (source ${SRC:0:8})"
desc() { curl -s -A curl "$CDN/releases/$SRC/windows.json" | python3 -c 'import json,sys;print(json.load(sys.stdin)["build"])' 2>/dev/null; }
run=""
for _ in $(seq 90); do   # Forgejo API and CDN only: nothing here touches the Certum session
    sleep 20
    run=$(api "$A/actions/runs?limit=20" | python3 -c "
import json,sys
for x in json.load(sys.stdin)['workflow_runs']:
    if x['workflow_id']=='windows-diagnostic.yml' and x['trigger_event']=='workflow_dispatch' and x['created']>='$since':
        print(x['index_in_repo'], x['status'], x['html_url']); break")
    b=$(desc)
    if [ -n "$b" ] && [ "$b" -gt "$BUILD" ]; then say "published 0.1.$b ($run)"; echo "$b"; exit 0; fi
    case "$run" in *" failure "*|*" cancelled "*) break ;; esac
done
say "BLOCKER: re-sign did not publish (${run:-no run found})"
exit 1
