#!/usr/bin/env bash
# Boot the test guest once. The guest runs `triad <args> --json`, sends one
# chat line, prints its memory figures to the serial log and powers off.
#
#   scripts/vm/run-vm.sh <x86_64|arm64> <mem-MB> <serial-log> <triad args...>
#   scripts/vm/run-vm.sh x86_64 64 /tmp/vm.log create --auto-accept
#
# Needs the output of scripts/vm/build.sh for the same architecture.
#
# Environment overrides:
#   VM_OUT       build output directory (default: <repo>/target/vm)
#   VM_WAIT      seconds the guest waits before sending its line
#   VM_SETTLE    seconds the guest waits after sending, before measuring
#   VM_TIMEOUT   seconds before QEMU is killed
#   VM_ENV       KEY=VALUE lines for triad's environment in the guest
#   VM_DATA      a directory whose files are copied into the guest's /data
#   VM_LINE      the chat line the guest sends (default "hello from the vm")
#   VM_DATA_DISK an ext4 image attached as a second disk and mounted on /data,
#                so the guest's data (and installed updates) outlive the run
#   VM_RESUME    triad arguments for a restart by the supervisor (default: chat)
#   VM_HOSTFWD   a QEMU hostfwd rule, e.g. tcp:127.0.0.1:17878-:7878, so this
#                machine can reach a port in the guest (serve-test.sh)
#
# With TRIAD_SUPERVISE=1 in VM_ENV the guest's init keeps triad running and
# updated instead of doing the test run; see init.c and update-test.sh.
#   VM_ACCEL     QEMU accelerator (default: kvm for x86_64, tcg for arm64)
#   VM_CPU       QEMU CPU model
#   QEMU         QEMU binary
set -euo pipefail

ARCH=${1:?usage: run-vm.sh <x86_64|arm64> <mem-MB> <serial-log> <triad args...>}
MEM=${2:?memory in MB}
LOG=${3:?serial log path}
shift 3

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
A=${VM_OUT:-$ROOT/target/vm}/$ARCH
[ -f "$A/kernel" ] || { echo "no build in $A; run scripts/vm/build.sh $ARCH first" >&2; exit 1; }

case $ARCH in
  x86_64)
    QEMU=${QEMU:-qemu-system-x86_64}
    ACCEL=${VM_ACCEL:-kvm}
    CPU=${VM_CPU:-host}
    MACHINE=microvm,acpi=off,x-option-roms=off,rtc=on
    CONSOLE=ttyS0
    EXTRA="reboot=t"
    WAIT=${VM_WAIT:-30} SETTLE=${VM_SETTLE:-8} TIMEOUT=${VM_TIMEOUT:-120}
    ;;
  arm64)
    QEMU=${QEMU:-qemu-system-aarch64}
    ACCEL=${VM_ACCEL:-tcg}
    CPU=${VM_CPU:-cortex-a72}
    MACHINE=virt
    CONSOLE=ttyAMA0
    EXTRA=""
    # Defaults assume software emulation, which is several times slower.
    WAIT=${VM_WAIT:-90} SETTLE=${VM_SETTLE:-20} TIMEOUT=${VM_TIMEOUT:-400}
    ;;
  *) echo "unknown architecture: $ARCH" >&2; exit 2 ;;
esac

# Fresh root filesystem for every run: the binaries plus an empty /data.
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK"/root/{bin,sbin,proc,sys,dev,tmp,etc,data}
cp "$A/triad" "$WORK/root/bin/triad"
cp "$A/init" "$WORK/root/sbin/init"
if [ -n "${VM_DATA:-}" ]; then cp -r "$VM_DATA"/. "$WORK/root/data/"; fi
echo "$*" >"$WORK/root/etc/triad.args"
echo "$WAIT $SETTLE" >"$WORK/root/etc/triad.wait"
printf '%s\n' "${VM_ENV:-}" >"$WORK/root/etc/triad.env"
printf '%s\n' "${VM_LINE:-}" >"$WORK/root/etc/triad.line"
printf '%s\n' "${VM_RESUME:-}" >"$WORK/root/etc/triad.resume"
DATA_DISK=()
if [ -n "${VM_DATA_DISK:-}" ]; then
  DATA_DISK=(-drive "id=data,file=$VM_DATA_DISK,format=raw,if=none" -device virtio-blk-device,drive=data)
fi
mkfs.ext4 -q -d "$WORK/root" -L root "$WORK/rootfs.ext4" 64M

timeout "$TIMEOUT" "$QEMU" -M "$MACHINE" -accel "$ACCEL" -cpu "$CPU" -m "$MEM" -smp 1 \
  -nodefaults -no-user-config -nographic -no-reboot \
  -kernel "$A/kernel" \
  -append "console=$CONSOLE root=/dev/vda rw ip=dhcp panic=-1 quiet $EXTRA" \
  -drive id=root,file="$WORK/rootfs.ext4",format=raw,if=none -device virtio-blk-device,drive=root \
  "${DATA_DISK[@]}" \
  -netdev "user,id=net${VM_HOSTFWD:+,hostfwd=$VM_HOSTFWD}" -device virtio-net-device,netdev=net \
  -device virtio-rng-device \
  -serial file:"$LOG"
