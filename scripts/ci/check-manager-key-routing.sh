#!/bin/bash
# No GUI activation or production state; safe on hosted macOS.
set -Eeuo pipefail
[[ $(uname -s) = Darwin ]]
fixture_dir=$(mktemp -d)
trap 'rm -rf "$fixture_dir"' EXIT
xcrun clang -fobjc-arc -fblocks -Wall -Werror -mmacosx-version-min=11.0 \
    -framework AppKit -framework WebKit \
    -I crates/okilum-shell/src/reader_manager_focus \
    scripts/qa/manager-key-routing.m -o "$fixture_dir/manager-key-routing"
"$fixture_dir/manager-key-routing"
