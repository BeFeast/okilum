#!/bin/bash
# Isolated hosted macOS fixture; never contacts T3 or opens a production profile.
set -Eeuo pipefail
[[ $(uname -s) = Darwin ]]
fixture_dir=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/okilum-webview-paste.XXXXXX")
trap 'rm -rf "$fixture_dir"' EXIT
xcrun clang -fobjc-arc -fblocks -Wall -Werror -mmacosx-version-min=11.0 \
  -framework AppKit -framework WebKit scripts/qa/manager-webview-paste.m \
  -o "$fixture_dir/manager-webview-paste"
"$fixture_dir/manager-webview-paste"
