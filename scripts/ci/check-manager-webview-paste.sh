#!/bin/bash
# Manual GUI-session fixture; never contacts T3 or opens a production profile.
set -Eeuo pipefail
[[ $(uname -s) = Darwin ]]
if [[ ${OKILUM_NATIVE_GUI:-0} != 1 ]]; then
  echo 'Requires an active macOS GUI session and OKILUM_NATIVE_GUI=1; not a hosted CI gate.' >&2
  exit 2
fi
fixture_dir=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/okilum-webview-paste.XXXXXX")
trap 'rm -rf "$fixture_dir"' EXIT
xcrun clang -fobjc-arc -fblocks -Wall -Werror -mmacosx-version-min=11.0 \
  -framework AppKit -framework WebKit scripts/qa/manager-webview-paste.m \
  -o "$fixture_dir/manager-webview-paste"
"$fixture_dir/manager-webview-paste"
