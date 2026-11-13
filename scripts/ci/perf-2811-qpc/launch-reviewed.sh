#!/usr/bin/env bash
# The manager runs this only AFTER final source/controller review. No force push.
set -euo pipefail
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(git -C "$script_dir" rev-parse --show-toplevel)
cd "$repo_root"
test -z "$(git status --porcelain)"
main_sha=$(python3 scripts/ci/perf-2811-qpc/provenance.py source --kind main)
toolbar_sha=$(python3 scripts/ci/perf-2811-qpc/provenance.py source --kind toolbar)
python3 scripts/ci/perf-2811-qpc/provenance.py verify --kind main --source "$main_sha"
python3 scripts/ci/perf-2811-qpc/provenance.py verify --kind toolbar --source "$toolbar_sha"
controller_sha=$(git rev-parse HEAD)
git -c credential.helper= -c 'credential.helper=!gh auth git-credential' push https://github.com/BeFeast/okilum.git \
    "$main_sha:refs/heads/diagnostic-toolbar-2811/qpc-source-main-${main_sha:0:12}" \
    "$toolbar_sha:refs/heads/diagnostic-toolbar-2811/qpc-source-toolbar-${toolbar_sha:0:12}"
# Only this controller prefix starts the new workflow. Sources are present first.
git -c credential.helper= -c 'credential.helper=!gh auth git-credential' push https://github.com/BeFeast/okilum.git \
    "$controller_sha:refs/heads/diagnostic-toolbar-2811/qpc-controller-${controller_sha:0:12}"
