#!/usr/bin/env bash
# Run unchanged artifact bytes in a fresh runtime container, without FUSE.
set -euo pipefail
case "$(. /etc/os-release; echo "$ID:$VERSION_ID")" in
    ubuntu:24.04)
        apt-get update -qq
        apt-get install -y -qq --no-install-recommends python3 file libvulkan1 mesa-vulkan-drivers \
            libfontconfig1 libfreetype6 libxcb1 libxkbcommon0 libxkbcommon-x11-0 \
            libwayland-client0 libglib2.0-0t64 libdbus-1-3 fonts-dejavu-core \
            xvfb xauth xdotool x11-utils dbus-x11 ;;
    fedora:*)
        dnf install -y python3 file vulkan-loader mesa-vulkan-drivers fontconfig freetype \
            libxcb libxkbcommon libxkbcommon-x11 libwayland-client glib2 dbus-libs \
            dejavu-sans-fonts xorg-x11-server-Xvfb xorg-x11-xauth xdotool xmessage dbus-x11 ;;
    *) echo 'Unexpected smoke runtime'; exit 1 ;;
esac
mkdir -p /work
cd /work
(cd /artifact && sha256sum -c SHA256SUMS)
/artifact/Okilum-x86_64.AppImage --appimage-extract
python3 /source/scripts/appimage/verify.py /artifact /work/squashfs-root
bash /source/scripts/appimage/launch.sh /artifact/Okilum-x86_64.AppImage --help
mkdir /work/bin
printf '#!/bin/sh\nexec bash /source/scripts/appimage/launch.sh /artifact/Okilum-x86_64.AppImage "$@"\n' > /work/bin/okilum
chmod +x /work/bin/okilum
export PATH="/work/bin:$PATH"
# Positive control: prove this X server and xdotool can detect a visible window.
xvfb-run -a -s '-screen 0 1280x800x24' dbus-run-session -- bash -euo pipefail -c '
    xmessage -name appimage-positive-control -timeout 10 control &
    control=$!
    trap '\''kill "$control" 2>/dev/null || true'\'' EXIT
    for i in $(seq 1 10); do
        if xdotool search --onlyvisible --name appimage-positive-control; then break; fi
        sleep 0.2
    done
    xdotool search --onlyvisible --name appimage-positive-control
    bash /source/scripts/arch/smoke-x11.sh
'
echo "APPIMAGE_NATIVE_PASS $(. /etc/os-release; echo "$PRETTY_NAME")"
