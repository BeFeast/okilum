#!/usr/bin/env bash
# Owner step for the Okilum Windows QA VM (#1113). Run from maestro as god:
#   bash create-vm.sh MEDIA_DIR
# MEDIA_DIR holds okilum-win-qa-answer.iso and okilum-win-qa-provision.iso (build-media.sh).
# Creates VM 150 okilum-win-qa on mimir, installs Windows 11 Enterprise Evaluation
# unattended (about 30-50 minutes), removes the install media, snapshots `clean` and
# starts the VM. VM 150 is the CI executor's QA guest (runbook infra-identity-discipline,
# section 1, exception of 2026-10-11): once it exists, every state-changing qm call checks
# the VM name in the same command and stops on a mismatch.
# Rollback: ssh mimir "qm config 150 | grep -qx 'name: okilum-win-qa' && { qm stop 150; qm destroy 150 --purge; }"
set -Eeuo pipefail
media="${1:?media directory required}"
VMID=150
WIN=Win11_Enterprise_Eval_25H2_en-us_x64.iso
ISO=/var/lib/vz/template/iso
NAME=okilum-win-qa  # Proxmox name; the Windows hostname is OKILUM-WIN-QA
# Prefix for every state-changing qm command on mimir after qm create: the VM must still be ours.
IS_OURS="qm config $VMID | grep -qx 'name: $NAME' || { echo 'VM $VMID is not $NAME, stopping' >&2; exit 3; }"
say() { echo "[$(date +%H:%M:%S)] $*"; }

# Identity before any write: the hosts that answer must be the ones named.
[ "$(ssh opti hostname)" = opti ] || { echo "opti answered as someone else" >&2; exit 1; }
[ "$(ssh mimir hostname)" = mimir ] || { echo "mimir answered as someone else" >&2; exit 1; }
ssh mimir "! qm status $VMID >/dev/null 2>&1 && ! pct status $VMID >/dev/null 2>&1" \
    || { echo "VMID $VMID already exists on mimir" >&2; exit 1; }

say "copying install media to mimir:$ISO (Windows ~7 GB from opti)"
ssh mimir "mkdir -p $ISO"
for name in $WIN virtio-win.iso; do
    ssh mimir "test -s $ISO/$name" || ssh opti "cat /mnt/storage/template/iso/$name" | ssh mimir "cat > $ISO/$name.part && mv $ISO/$name.part $ISO/$name"
done
scp -q "$media/okilum-win-qa-answer.iso" "$media/okilum-win-qa-provision.iso" mimir:$ISO/
ssh mimir "chmod 600 $ISO/okilum-win-qa-answer.iso"

say "creating VM $VMID okilum-win-qa"
ssh mimir "qm create $VMID --name $NAME \
  --description 'Okilum Windows QA (BeFeast/okilum#1113). Snapshot clean = fresh provisioned Windows; QA runs start from it.' \
  --ostype win11 --machine q35 --bios ovmf --cpu host --sockets 1 --cores 4 --cpuunits 50 \
  --memory 8192 --balloon 0 --scsihw virtio-scsi-single --vga std --tablet 1 --agent enabled=1 \
  --efidisk0 local-zfs:1,efitype=4m,pre-enrolled-keys=1 --tpmstate0 local-zfs:1,version=v2.0 \
  --sata0 local-zfs:80,ssd=1,discard=on --net0 e1000,bridge=vmbr0 \
  --ide2 local:iso/$WIN,media=cdrom --ide0 local:iso/virtio-win.iso,media=cdrom \
  --sata1 local:iso/okilum-win-qa-answer.iso,media=cdrom --sata2 local:iso/okilum-win-qa-provision.iso,media=cdrom \
  --boot 'order=sata0;ide2' --onboot 0"

say "starting; pressing Enter for the UEFI 'boot from CD' prompt"
ssh mimir "set -e; $IS_OURS; qm start $VMID; for i in \$(seq 20); do sleep 1; qm sendkey $VMID ret; done"

say "unattended install and provisioning; the VM powers itself off when done (checked every minute, up to 90 min)"
for i in $(seq 90); do
    sleep 60
    state=$(ssh mimir "qm status $VMID" | awk '{print $2}')
    [ "$state" = stopped ] && break
    [ $((i % 5)) = 0 ] && say "still installing ($i min)"
done
[ "$state" = stopped ] || { echo "the VM did not finish within 90 minutes; it is left running for inspection" >&2; exit 1; }

say "removing install media (the answer CD carries the qa password) and snapshotting clean"
ssh mimir "set -e; $IS_OURS; qm set $VMID --delete sata1,sata2,ide0,ide2; rm -f $ISO/okilum-win-qa-answer.iso; \
  $IS_OURS; qm snapshot $VMID clean --description 'Provisioned Windows 11, before any Okilum install (#1113)'; \
  $IS_OURS; qm start $VMID"
sleep 120
say "VM $VMID started from the clean state; addresses reported by the guest agent:"
ssh mimir "qm guest cmd $VMID network-get-interfaces" | grep -o '"ip-address" : "10\.[0-9.]*"' || echo "(guest agent not up yet)"
