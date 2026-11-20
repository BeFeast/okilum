# Shared by the pool gate scripts. Source this file; do not execute it.
# A phase records {name, code} into $OKILUM_POOL_RECEIPT. The first non-zero
# phase stops the rest, except the fixture reap, which always runs.
pool_skip=0
pool_fail=0
pool_fail_infra=0

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
    if [[ $code -ne 0 && $pool_fail -eq 0 ]]; then
        pool_fail=$code
        pool_skip=1
        case $code in
            75|124) pool_fail_infra=1 ;;
        esac
        case $name in
            "Verify pinned Rust toolchain"|"Fetch locked public dependencies") pool_fail_infra=1 ;;
        esac
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
    if [[ $pool_fail_infra -eq 1 ]]; then
        exit 75
    fi
    exit 1
}
