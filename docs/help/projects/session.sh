#!/usr/bin/env bash
# Private tutorial session: isolated state, synthetic practice vault, own Xvfb display.
# Run under: flock ~/.okilum-ui.lock session.sh <take-name>
set -euo pipefail
Q=/home/god/work/qa-evidence/issue-2356
TAKE=${1:-explore}
W=$Q/$TAKE
rm -rf "$W"; mkdir -p "$W"
for d in home config cache state runtime; do mkdir -m 700 "$W/$d"; done
V=${TMPDIR:-/var/tmp}/okilum-tutorial/practice-vault
rm -rf "$V"; mkdir -p "$V"; ln -s "$V" "$W/practice-vault"
mkdir -p "$V/Notes" "$V/Kitchen remodel/decisions" "$V/Kitchen remodel/reports"
cat > "$V/Kitchen remodel/decisions/2026-11-10-quartz-countertops.md" <<'EOF2'
# Choose quartz countertops

Quartz resists stains and needs no sealing. Order samples before the final colour.
EOF2
cat > "$V/Kitchen remodel/reports/2026-11-12-budget-check.md" <<'EOF2'
# Budget check

Cabinets and lighting fit the plan. Countertops are still an estimate.
EOF2
cat > "$V/Notes/Welcome.md" <<'EOF'
# Welcome

A practice vault for trying Projects. Notes stay ordinary Markdown files.
EOF
cat > "$V/Notes/Reading list.md" <<'EOF'
# Reading list

- The Design of Everyday Things
- A Pattern Language
EOF
cat > "$V/Kitchen remodel/Ideas.md" <<'EOF'
# Kitchen remodel ideas

- Open shelving by the window
- Warm under-cabinet lights
EOF
DISPLAY_NUM=${DISPLAY_NUM:-57}
__EGL_VENDOR_LIBRARY_FILENAMES=/usr/share/glvnd/egl_vendor.d/50_mesa.json Xvfb ":$DISPLAY_NUM" -screen 0 1440x960x24 -nolisten tcp >"$W/xvfb.log" 2>&1 &
XPID=$!
trap 'kill $APID 2>/dev/null || true; sleep 1; kill $XPID 2>/dev/null || true' EXIT
sleep 2
env -i PATH=/usr/bin:/bin HOME="$W/home" XDG_CONFIG_HOME="$W/config" XDG_CACHE_HOME="$W/cache" \
  XDG_RUNTIME_DIR="$W/runtime" VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json __EGL_VENDOR_LIBRARY_FILENAMES=/usr/share/glvnd/egl_vendor.d/50_mesa.json OKILUM_STATE_DIR="$W/state" DISPLAY=":$DISPLAY_NUM" LANG=en_US.UTF-8 \
  "$Q/Okilum-x86_64.AppImage" --appimage-extract-and-run --vault "$V" --note "Notes/Welcome.md" >"$W/app.log" 2>&1 &
APID=$!
echo "session $TAKE display :$DISPLAY_NUM app $APID xvfb $XPID"
if [[ -n "${DRIVER:-}" ]]; then
  until DISPLAY=":$DISPLAY_NUM" xdotool search --onlyvisible --name Okilum >/dev/null 2>&1; do sleep 1; done
  sleep 8
  DISPLAY=":$DISPLAY_NUM" WORK="$W" python3 "$DRIVER" && echo "driver ok" || echo "driver failed"
  exit 0
fi
# Stop when the stop file appears (or after 2 h).
for _ in $(seq 1 7200); do
  [[ -e "$W/STOP" ]] && break
  kill -0 $APID 2>/dev/null || { echo "app exited"; break; }
  sleep 1
done
