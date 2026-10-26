#!/usr/bin/env bash
# Hosted-only runtime. No source compilation. The caller downloads two retained
# artifacts first; this container receives them read-only and runs unprivileged.
set -euo pipefail
case "$(. /etc/os-release; echo "$ID:$VERSION_ID")" in
  ubuntu:24.04)
    apt-get update -qq
    apt-get install -y -qq --no-install-recommends python3 gnupg gpgv libvulkan1 mesa-vulkan-drivers \
      libfontconfig1 libfreetype6 libxcb1 libxkbcommon0 libxkbcommon-x11-0 libwayland-client0 \
      libglib2.0-0t64 libdbus-1-3 fonts-dejavu-core xvfb xauth xdotool x11-utils x11-apps dbus-x11 ;;
  fedora:*)
    dnf install -y python3 gnupg2 vulkan-loader mesa-vulkan-drivers fontconfig freetype libxcb \
      libxkbcommon libxkbcommon-x11 libwayland-client glib2 dbus-libs dejavu-sans-fonts \
      xorg-x11-server-Xvfb xorg-x11-xauth xdotool xmessage dbus-x11 shadow-utils ;;
  *) echo 'Unexpected acceptance runtime'; exit 1 ;;
esac
useradd --create-home --uid 12345 acceptance
mkdir -p /work /evidence
chown acceptance /work /evidence
# Explicit single-runtime desktop-only path; leave the pair gate defaults intact.
if [[ ${OKILUM_DESKTOP_ONLY:-} == 1716 ]]; then
  [[ $(. /etc/os-release; echo "$ID:$VERSION_ID") == ubuntu:24.04 ]]
  exec timeout --signal=TERM --kill-after=20s 15m runuser -u acceptance -- \
    xvfb-run -a -s '-screen 0 1280x800x24' dbus-run-session -- \
    python3 /source/scripts/appimage/desktop-lifecycle.py
fi
# Artifact extraction requires writable executable paths but never alters bytes.
cp -a /old /work/old
cp -a /new /work/new
chown -R acceptance /work
chmod +x /work/{old,new}/Okilum-x86_64.AppImage
exec runuser -u acceptance -- xvfb-run -a -s '-screen 0 1280x800x24' dbus-run-session -- \
  python3 /source/scripts/appimage/update-acceptance.py \
    --old /work/old --new /work/new --evidence /evidence/native --gui
