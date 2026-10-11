#!/usr/bin/env bash
# Attach sindri's Iris Xe to VM 151 okilum-win-gpu (#1264). Run from maestro as god, after
# create-vm.sh with QA_VM=gpu and after the sindri reboot that hands 0000:00:02.0 to vfio-pci:
#   bash gpu-passthrough.sh
# Host prep (once, before that reboot): /etc/modprobe.d/okilum-igd-vfio.conf
# (vfio-pci ids=8086:9a49, softdep i915/xe pre: vfio-pci), vfio modules in /etc/modules,
# GRUB cmdline "intel_iommu=on iommu=pt initcall_blacklist=sysfb_init". Undo with
# /root/okilum-igd-rollback.sh on sindri, then reboot.
#
# Tiger Lake is Gen12: QEMU's legacy IGD mode is automatic only up to Gen9, so the device is
# placed by hand the way legacy mode would: guest 00:02.0 on i440fx, no emulated VGA, the
# host LPC/host-bridge IDs copied (x-igd-lpc, i440fx only), OpRegion passed (QEMU default),
# and an Intel GOP option ROM so OVMF and Windows can drive the panel from boot.
# ROM: RKL_TGL_ADL_RPL_GOPv17_igd.rom from github.com/LongQT-sea/intel-igpu-passthru v0.1.
set -Eeuo pipefail
HOST=sindri VMID=151 NAME=okilum-win-gpu GPU=0000:00:02.0
ROM=okilum-tgl-gopv17-igd.rom
ROM_URL=https://github.com/LongQT-sea/intel-igpu-passthru/releases/download/v0.1/RKL_TGL_ADL_RPL_GOPv17_igd.rom
ROM_SHA256=9d91bb6a5cc6d7c6dc4ab1783771b77972bf00c17e05d58850638eb389ff1ce4
IS_OURS="qm config $VMID | grep -qx 'name: $NAME' || { echo 'VM $VMID is not $NAME, stopping' >&2; exit 3; }"
say() { echo "[$(date +%H:%M:%S)] $*"; }

[ "$(ssh "$HOST" hostname)" = "$HOST" ] || { echo "$HOST answered as someone else" >&2; exit 1; }
driver=$(ssh "$HOST" "basename \$(readlink /sys/bus/pci/devices/$GPU/driver) 2>/dev/null || echo none")
[ "$driver" = vfio-pci ] || { echo "$GPU is bound to $driver, not vfio-pci: reboot sindri after the host prep" >&2; exit 1; }

say "option ROM /usr/share/kvm/$ROM"
ssh "$HOST" "set -e; f=/usr/share/kvm/$ROM; [ -s \$f ] || { curl -fsSL -o \$f.part $ROM_URL; mv -f \$f.part \$f; }; \
  echo '$ROM_SHA256  '\$f | sha256sum -c --quiet"

say "VM $VMID: Iris Xe at guest 00:02.0, no emulated VGA"
ssh "$HOST" "set -e; $IS_OURS; [ \"\$(qm status $VMID | awk '{print \$2}')\" = stopped ] || { echo 'stop VM $VMID first' >&2; exit 4; }; \
  $IS_OURS; qm set $VMID --machine pc --vga none --hostpci0 $GPU,legacy-igd=1,romfile=$ROM \
    --args '-set device.hostpci0.x-igd-lpc=on'"
ssh "$HOST" "qm config $VMID | grep -E '^(machine|vga|hostpci0|args|bios):'"
ssh "$HOST" "qm showcmd $VMID --pretty | grep -E 'vfio-pci|igd|romfile'"
