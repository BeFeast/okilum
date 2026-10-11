#!/usr/bin/env bash
# Create an Okilum Windows QA VM (#1113, #1264). Run from maestro as god:
#   bash create-vm.sh MEDIA_DIR                 # VM 150 okilum-win-qa on mimir (defaults)
#   QA_VM=gpu bash create-vm.sh MEDIA_DIR       # VM 151 okilum-win-gpu on sindri, no GPU yet
# MEDIA_DIR holds okilum-win-qa-answer.iso and okilum-win-qa-provision.iso (build-media.sh;
# for the GPU VM built with COMPUTER_NAME=OKILUM-WIN-GPU).
# Installs Windows 11 Enterprise Evaluation unattended (about 30-50 minutes), removes the
# install media, snapshots `clean` and starts the VM. The GPU VM installs on emulated VGA and
# its snapshot is `base`; gpu-passthrough.sh attaches the Iris Xe, and `clean` follows the
# Intel driver.
# Both VMs are the CI executor's QA guests (runbook infra-identity-discipline, section 1,
# exception of 2026-10-11): once the VM exists, every state-changing qm call checks the VM
# name in the same command and stops on a mismatch.
# Rollback: ssh $HOST "qm config $VMID | grep -qx 'name: $NAME' && { qm stop $VMID; qm destroy $VMID --purge; }"
set -Eeuo pipefail
media="${1:?media directory required}"
case "${QA_VM:-qa}" in
qa)  # mimir, ZFS; Secure Boot on
    HOST=mimir VMID=150 NAME=okilum-win-qa ISSUE=1113 MACHINE=q35 KEYS=1 SNAP=clean
    DISK=local-zfs:80,ssd=1,discard=on SMALL=local-zfs ;;
gpu) # sindri: local-lvm is nearly full and shared with CT140/141, so the system disk is
     # qcow2 on `local`; EFI vars and TPM state are tiny and need snapshot-capable storage.
     # i440fx, because the Tiger Lake IGD needs x-igd-lpc, which QEMU supports there only;
     # Secure Boot off, because the Intel GOP ROM is unsigned.
    HOST=sindri VMID=151 NAME=okilum-win-gpu ISSUE=1264 MACHINE=pc KEYS=0 SNAP=base
    DISK=local:64,format=qcow2,ssd=1,discard=on SMALL=local-lvm ;;
*) echo "QA_VM must be qa or gpu" >&2; exit 1 ;;
esac
WIN=Win11_Enterprise_Eval_25H2_en-us_x64.iso
ISO=/var/lib/vz/template/iso
# Prefix for every state-changing qm command after qm create: the VM must still be ours.
IS_OURS="qm config $VMID | grep -qx 'name: $NAME' || { echo 'VM $VMID is not $NAME, stopping' >&2; exit 3; }"
say() { echo "[$(date +%H:%M:%S)] $*"; }

# Identity before any write: the hosts that answer must be the ones named.
[ "$(ssh opti hostname)" = opti ] || { echo "opti answered as someone else" >&2; exit 1; }
[ "$(ssh "$HOST" hostname)" = "$HOST" ] || { echo "$HOST answered as someone else" >&2; exit 1; }
ssh "$HOST" "! qm status $VMID >/dev/null 2>&1 && ! pct status $VMID >/dev/null 2>&1" \
    || { echo "VMID $VMID already exists on $HOST" >&2; exit 1; }

say "copying install media to $HOST:$ISO (Windows ~7 GB from opti)"
ssh "$HOST" "mkdir -p $ISO"
for name in $WIN virtio-win.iso; do
    ssh "$HOST" "test -s $ISO/$name" || ssh opti "cat /mnt/storage/template/iso/$name" | ssh "$HOST" "cat > $ISO/$name.part && mv $ISO/$name.part $ISO/$name"
done
scp -q "$media/okilum-win-qa-answer.iso" "$media/okilum-win-qa-provision.iso" "$HOST:$ISO/"
ssh "$HOST" "chmod 600 $ISO/okilum-win-qa-answer.iso"

say "creating VM $VMID $NAME on $HOST"
ssh "$HOST" "qm create $VMID --name $NAME \
  --description 'Okilum Windows QA (BeFeast/okilum#$ISSUE). Snapshot clean = fresh provisioned Windows; QA runs start from it.' \
  --ostype win11 --machine $MACHINE --bios ovmf --cpu host --sockets 1 --cores 4 --cpuunits 50 \
  --memory 8192 --balloon 0 --scsihw virtio-scsi-single --vga std --tablet 1 --agent enabled=1 \
  --efidisk0 $SMALL:1,efitype=4m,pre-enrolled-keys=$KEYS --tpmstate0 $SMALL:1,version=v2.0 \
  --sata0 $DISK --net0 e1000,bridge=vmbr0 \
  --ide2 local:iso/$WIN,media=cdrom --ide0 local:iso/virtio-win.iso,media=cdrom \
  --sata1 local:iso/okilum-win-qa-answer.iso,media=cdrom --sata2 local:iso/okilum-win-qa-provision.iso,media=cdrom \
  --boot 'order=sata0;ide2' --onboot 0"

say "starting; pressing Enter for the UEFI 'boot from CD' prompt"
ssh "$HOST" "set -e; $IS_OURS; qm start $VMID; for i in \$(seq 20); do sleep 1; qm sendkey $VMID ret; done"

say "unattended install and provisioning; the VM powers itself off when done (checked every minute, up to 90 min)"
for i in $(seq 90); do
    sleep 60
    state=$(ssh "$HOST" "qm status $VMID" | awk '{print $2}')
    [ "$state" = stopped ] && break
    [ $((i % 5)) = 0 ] && say "still installing ($i min)"
done
[ "$state" = stopped ] || { echo "the VM did not finish within 90 minutes; it is left running for inspection" >&2; exit 1; }

say "removing install media (the answer CD carries the qa password) and snapshotting $SNAP"
ssh "$HOST" "set -e; $IS_OURS; qm set $VMID --delete sata1,sata2,ide0,ide2; rm -f $ISO/okilum-win-qa-answer.iso; \
  $IS_OURS; qm snapshot $VMID $SNAP --description 'Provisioned Windows 11, before any Okilum install (#$ISSUE)'; \
  $IS_OURS; qm start $VMID"
sleep 120
say "VM $VMID started from the $SNAP state; addresses reported by the guest agent:"
ssh "$HOST" "qm guest cmd $VMID network-get-interfaces" | grep -o '"ip-address" : "10\.[0-9.]*"' || echo "(guest agent not up yet)"
