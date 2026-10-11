#!/usr/bin/env bash
# Finish an Okilum Windows QA VM whose first-logon provisioning stopped part way (#1113).
# Run from maestro as god, the owner's single step:
#   bash finish-vm.sh GUEST_IP
# Windows is installed and qa is logged in; this copies the fixed provisioning over SSH
# (qa's password from Infisical) and runs it from C:\qa-setup. Provisioning powers the VM
# off, then the install media and the answer CD are removed, `clean` is snapshotted, and
# the VM starts. Agents do not run qm (runbook infra-identity-discipline, section 1).
# Rollback (nothing kept): ssh mimir 'qm stop 150; qm destroy 150 --purge; rm -f /var/lib/vz/template/iso/okilum-win-qa-*.iso'
set -Eeuo pipefail
ip="${1:?guest IP required}"
VMID=150
ISO=/var/lib/vz/template/iso
here="$(cd "$(dirname "$0")" && pwd)"
say() { echo "[$(date +%H:%M:%S)] $*"; }

[ "$(ssh mimir hostname)" = mimir ] || { echo "mimir answered as someone else" >&2; exit 1; }
ssh mimir "qm status $VMID" | grep -q running || { echo "VM $VMID is not running" >&2; exit 1; }
SSHPASS="$(secret get infrastructure /okilum-win-qa QA_USER_PASSWORD)"; export SSHPASS
guest() { sshpass -e ssh -o PubkeyAuthentication=no -o PreferredAuthentications=password,keyboard-interactive \
    -o StrictHostKeyChecking=accept-new -o ConnectTimeout=10 "qa@$ip" "$@"; }
[ "$(guest hostname | tr -d '\r')" = OKILUM-WIN-QA ] || { echo "$ip is not OKILUM-WIN-QA" >&2; exit 1; }

say "copying fixed provisioning to C:\\qa-setup"
guest 'mkdir C:\qa-setup 2>nul & exit 0'
sshpass -e scp -q -o PubkeyAuthentication=no -o PreferredAuthentications=password,keyboard-interactive \
    "$here/provision.ps1" "$here/run-gui.ps1" "$here/authorized_keys" "qa@$ip:C:/qa-setup/"

say "running provisioning in qa's console session (it restarts sshd, then powers the VM off)"
# A scheduled task, not this SSH session: provision.ps1 restarts sshd, which ends SSH sessions.
# Encoded, because the guest's SSH shell is still cmd.exe and would eat quotes and redirects.
task='$a = New-ScheduledTaskAction -Execute powershell.exe -Argument "-NoProfile -ExecutionPolicy Bypass -Command & C:\qa-setup\provision.ps1 -Media C:\qa-setup *> C:\okilum-qa-provision-finish.log"; $p = New-ScheduledTaskPrincipal -UserId qa -LogonType Interactive -RunLevel Highest; Register-ScheduledTask -TaskName okilum-qa-finish -Action $a -Principal $p -Force | Out-Null; Start-ScheduledTask -TaskName okilum-qa-finish'
guest "powershell -NoProfile -EncodedCommand $(printf '%s' "$task" | iconv -t UTF-16LE | base64 -w0)" >/dev/null
for i in $(seq 30); do
    sleep 20
    [ "$(ssh mimir "qm status $VMID" | awk '{print $2}')" = stopped ] && break
done
[ "$(ssh mimir "qm status $VMID" | awk '{print $2}')" = stopped ] || {
    echo "provisioning did not power off within 10 minutes; log: C:\\okilum-qa-provision-finish.log" >&2; exit 1; }

say "removing install media (the answer CD carries the qa password) and snapshotting clean"
ssh mimir "qm set $VMID --delete sata1,sata2,ide0,ide2 && rm -f $ISO/okilum-win-qa-answer.iso && \
  qm snapshot $VMID clean --description 'Provisioned Windows 11, before any Okilum install (#1113)' && qm start $VMID"
sleep 90
unset SSHPASS
say "checking key login and the guest agent"
ssh -o BatchMode=yes -o ConnectTimeout=10 -i ~/.ssh/id_ed25519_maestro "qa@$ip" 'whoami; type C:\qa\provisioned.txt' || echo "key login not up yet"
ssh mimir "qm agent $VMID ping" && echo "guest agent answers" || echo "guest agent not answering yet"
