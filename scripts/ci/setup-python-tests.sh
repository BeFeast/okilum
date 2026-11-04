#!/usr/bin/env bash
# Each job gets the same pinned dependencies, independent of host site-packages.
set -euo pipefail
: "${GITHUB_PATH:?setup-python-tests.sh requires the Actions GITHUB_PATH file}"
test_env=$(mktemp -d)
trap 'status=$?; if [ "$status" -ne 0 ]; then rm -rf -- "$test_env"; fi' EXIT
python3 -m venv "$test_env"
"$test_env/bin/python3" -I -m pip --isolated install --disable-pip-version-check \
    -r "$(dirname "$0")/requirements.txt"
"$test_env/bin/python3" -I "$(dirname "$0")/python_dependencies.py"
printf '%s\n' "$test_env/bin" >> "$GITHUB_PATH"
