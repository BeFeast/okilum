#!/usr/bin/env bash
# Operator setup on baldr, outside PR jobs. Applies live and survives reboot.
set -euo pipefail
[[ $(hostname -s) == baldr ]] || { echo 'Run on baldr only' >&2; exit 1; }
unit=forgejo-runner-fast.service
systemctl is-active --quiet "$unit"
before=$(systemctl show "$unit" --property=MainPID --value)
# Relative CPU share among system.slice siblings, including release containers.
# No affinity/quota, runner restart, or change to other services is needed.
sudo -n systemctl set-property "$unit" CPUWeight=1000
[[ $(systemctl show "$unit" --property=CPUWeight --value) == 1000 ]]
[[ $(systemctl show "$unit" --property=MainPID --value) == "$before" ]]
echo "FAST_RUNNER_PRIORITY=1000 pid=$before (no restart)"
