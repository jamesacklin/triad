#!/usr/bin/env bash
# End-to-end check: a triad on this machine creates a group, a triad inside
# the VM joins it, and each side sends one message. Exits 0 only if both
# sides saw both messages and the guest triad exited cleanly.
#
#   scripts/vm/chat-test.sh <x86_64|arm64> <mem-MB>
#
# `triad` must be on PATH for the host side (or set TRIAD), as in
# scripts/cross-test.sh. Results are kept in target/vm/<arch>/runs/<mem>/.
set -uo pipefail

ARCH=${1:?usage: chat-test.sh <x86_64|arm64> <mem-MB>}
MEM=${2:?memory in MB}
TRIAD=${TRIAD:-triad}

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
RUN=${VM_OUT:-$ROOT/target/vm}/$ARCH/runs/$MEM
rm -rf "$RUN"; mkdir -p "$RUN"

log() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*" >&2; }

# Host side: the creator, with stdin held open through a FIFO.
mkfifo "$RUN/host.in"
"$TRIAD" --data-dir "$RUN/host-data" create --no-bot --auto-accept --json \
  <"$RUN/host.in" >"$RUN/host.out" 2>"$RUN/host.err" &
HOST_PID=$!
exec 3>"$RUN/host.in"

for _ in $(seq 60); do grep -q '"event":"ready"' "$RUN/host.out" && break; sleep 0.5; done
INVITE=$(sed -n 's/.*"invite":"\([^"]*\)".*/\1/p' "$RUN/host.out" | head -1)
[ -n "$INVITE" ] || { log "FAIL: host never printed an invite"; kill "$HOST_PID"; exit 1; }

# Once the guest has joined, the host sends its line.
(
  for _ in $(seq 600); do grep -q '"event":"joined"' "$RUN/host.out" && break; sleep 0.5; done
  sleep 2
  echo "hello from the host" >&3
) &

log "booting $ARCH guest with ${MEM} MB"
"$HERE/run-vm.sh" "$ARCH" "$MEM" "$RUN/vm.log" join "$INVITE" --no-bot
QEMU_EXIT=$?

sleep 1
echo /quit >&3
wait "$HOST_PID"; HOST_EXIT=$?
exec 3>&-

count() { grep -c '"event":"message"' "$1" 2>/dev/null || true; }
HOST_MSGS=$(count "$RUN/host.out")
VM_MSGS=$(count "$RUN/vm.log")

echo "arch=$ARCH mem=${MEM}MB host_messages=$HOST_MSGS vm_messages=$VM_MSGS qemu_exit=$QEMU_EXIT host_exit=$HOST_EXIT"
grep -E 'MemTotal|MemFree|MemAvailable|VmHWM|RssAnon|TRIAD_EXIT|Out of memory' "$RUN/vm.log" | cut -c1-120

if [ "$HOST_MSGS" = 2 ] && [ "$VM_MSGS" = 2 ] && grep -q '^TRIAD_EXIT=0' "$RUN/vm.log"; then
  log "PASS"
else
  log "FAIL (logs in $RUN)"
  exit 1
fi
