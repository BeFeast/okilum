#!/usr/bin/env bash
# The signer desktop on :1: a window manager, a tray for SimplySign's icon, and SimplySign
# Desktop itself. Restarted by systemd if SimplySign exits; the owner logs in over VNC.
set -u
export DISPLAY=:1
eval "$(dbus-launch --sh-syntax)"
openbox &
stalonetray --geometry 4x1-0+0 --icon-size 24 &
cd /opt/SimplySignDesktop
exec ./SimplySignDesktop_start
