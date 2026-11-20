# Shared by the pool gate scripts. Source this file; do not execute it.
# A phase records {name, code} into $OKILUM_POOL_RECEIPT. The first non-zero
# phase stops the rest, except the fixture reap, which always runs.
pool_skip=0
pool_fail=0
pool_fail_infra=0

# git archive has no repository, and check-vendor-generation.py runs git ls-files.
pool_ensure_git() {
    # The slot directory is reused. A previous index can list files the new
    # archive does not have, or omit files a host excludesFile hid. When the
    # dispatcher names OKILUM_SOURCE_COMMIT, this tree is the slot copy:
    # rebuild every time. A developer clone (no such variable, and not one of
    # our synthetic commits) is left untouched.
    if [[ -d .git ]]; then
        local head subject
        head=$(git rev-parse HEAD 2>/dev/null || true)
        subject=$(git log -1 --format=%s 2>/dev/null || true)
        if [[ -n ${OKILUM_SOURCE_COMMIT:-} && $head == "$OKILUM_SOURCE_COMMIT" ]]; then
            return 0
        fi
        if [[ -z ${OKILUM_SOURCE_COMMIT:-} && $subject != 'pool source' && ! -f .git/okilum-pool-source ]]; then
            return 0
        fi
        rm -rf .git
    fi
    git init -q
    : > .git/okilum-pool-source
    # A host excludesFile such as **/credentials.* must not drop archived source.
    git -c core.excludesFile=/dev/null add -A
    git -c user.email=pool@okilum.local -c user.name='okilum pool' \
        -c core.excludesFile=/dev/null commit -q -m 'pool source'
}

# build-slot only ships directories under these roots back to the caller.
pool_receipt_dir() {
    local root=/home/god/scratch/okilum-pool-receipts
    mkdir -p "$root"
    mktemp -d "$root/receipt.XXXXXX"
}

pool_receipt_init() {
    [[ -n ${OKILUM_POOL_RECEIPT:-} ]] || { echo "OKILUM_POOL_RECEIPT is unset" >&2; exit 75; }
    mkdir -p "$(dirname "$OKILUM_POOL_RECEIPT")"
    printf '[]\n' > "$OKILUM_POOL_RECEIPT"
}

pool_record() {
    python3 - "$OKILUM_POOL_RECEIPT" "$1" "$2" <<'PY'
import json, sys
path, name, code = sys.argv[1], sys.argv[2], int(sys.argv[3])
with open(path) as handle:
    phases = json.load(handle)
phases.append({"name": name, "code": code})
with open(path, "w") as handle:
    json.dump(phases, handle)
    handle.write("\n")
PY
}

pool_phase() {
    local name=$1
    shift
    if [[ $pool_skip -eq 1 && $name != "Reject and reap surviving agent fixture daemons" ]]; then
        return 0
    fi
    printf '\nPOOL PHASE %s\n' "$name"
    set +e
    "$@"
    local code=$?
    set -e
    pool_record "$name" "$code"
    # A phase's own deadline (124) or a fetch failure is a code verdict.
    # Only build-slot's 75/124, with no failing phase, is infrastructure.
    if [[ $code -ne 0 && $pool_fail -eq 0 ]]; then
        pool_fail=$code
        pool_skip=1
    fi
}

pool_prepare_python() {
    # setup-python-tests.sh records the venv on GITHUB_PATH. Inside a slot that
    # file does not exist, so point it at a scratch file and then use the venv.
    local path_file
    path_file=$(mktemp)
    GITHUB_PATH=$path_file bash scripts/ci/setup-python-tests.sh
    if [[ -s $path_file ]]; then
        export PATH="$(head -n 1 "$path_file"):${PATH}"
    fi
    rm -f "$path_file"
}

pool_finish() {
    if [[ $pool_fail -eq 0 ]]; then
        exit 0
    fi
    exit 1
}
