#!/bin/sh
# Invoke on CT119 before its PBS snapshot. Any failure must stop that backup.
set -eu
cd /opt/tessera-inbox/source/inbox/deploy
exec sudo -n docker compose exec -T inbox /usr/local/bin/inbox-backup
