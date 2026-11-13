#!/usr/bin/env bash
# Root runs this only after reviewing setup repair. No build or source push.
set -euo pipefail
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(git -C "$script_dir" rev-parse --show-toplevel)
cd "$repo_root"
test -z "$(git status --porcelain)"
controller_sha=$(git rev-parse HEAD)
git -c credential.helper= -c 'credential.helper=!gh auth git-credential' push https://github.com/BeFeast/okilum.git \
    "$controller_sha:refs/heads/diagnostic-toolbar-2811/capture-only-${controller_sha:0:12}"
