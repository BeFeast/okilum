#!/usr/bin/env bash
# Native packaged-app startup and Ctrl+Q, under Xvfb + software Vulkan.
set -euo pipefail
smoke_dir=$(mktemp -d)
trap 'if [ -n "${app_pid:-}" ]; then kill "$app_pid" 2>/dev/null || true; fi; rm -rf "$smoke_dir"' EXIT
export XDG_RUNTIME_DIR="$smoke_dir/runtime"
mkdir -m700 "$XDG_RUNTIME_DIR"
mkdir "$smoke_dir/vault"
printf '# Linux smoke test\n\nA real Reader window.\n' > "$smoke_dir/vault/start.md"
tessera --vault "$smoke_dir/vault" --note start.md --index-dir "$smoke_dir/index" > "$smoke_dir/app.log" 2>&1 &
app_pid=$!
window=''
for _ in $(seq 1 60); do
  if ! kill -0 "$app_pid" 2>/dev/null; then cat "$smoke_dir/app.log"; exit 1; fi
  window=$(xdotool search --onlyvisible --class tessera 2>/dev/null | head -1 || true)
  [ -z "$window" ] || break
  sleep 1
done
if [ -z "$window" ]; then cat "$smoke_dir/app.log"; echo 'No visible Tessera window'; exit 1; fi
xdotool windowfocus --sync "$window"
xdotool key --clearmodifiers ctrl+q
for _ in $(seq 1 15); do
  if ! kill -0 "$app_pid" 2>/dev/null; then
    wait "$app_pid"
    app_pid=''
    echo 'Packaged Tessera opened a visible X11 window and exited cleanly via Ctrl+Q'
    exit 0
  fi
  sleep 1
done
cat "$smoke_dir/app.log"
echo 'Ctrl+Q did not exit Tessera'
exit 1
