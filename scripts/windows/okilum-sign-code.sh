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
