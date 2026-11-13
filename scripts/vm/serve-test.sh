#!/usr/bin/env bash
# The WebSocket API in the VM (WEBSOCKET-PLAN.md, W5): the guest runs
# `triad serve` (bots on, mock brain, so it starts on its DM); a triad on
# this machine creates a channel over a pipe; a socket client on this
# machine (ws-chat.py), through a QEMU port forward, has the guest join the
# channel, sees the host's line and answers it. Exits 0 only if both sides
# saw both messages and the guest triad exited cleanly.
#
#   scripts/vm/serve-test.sh <x86_64|arm64> <mem-MB>
#
# `triad` must be on PATH for the host side (or set TRIAD); python3 runs
# the socket client. Results are kept in target/vm/<arch>/serve-runs/<mem>/.
# WS_PORT picks the port on this machine (default 17878).
set -uo pipefail

ARCH=${1:?usage: serve-test.sh <x86_64|arm64> <mem-MB>}
MEM=${2:?memory in MB}
TRIAD=${TRIAD:-triad}
WS_PORT=${WS_PORT:-17878}

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
RUN=${VM_OUT:-$ROOT/target/vm}/$ARCH/serve-runs/$MEM
rm -rf "$RUN"; mkdir -p "$RUN/guest-data"

log() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*" >&2; }

# The guest's token, made here so this side can read it.
(umask 077; od -An -tx1 -N32 /dev/urandom | tr -d ' \n' >"$RUN/guest-data/api-token")
chmod 600 "$RUN/guest-data/api-token"

# Host side: the creator, with stdin held open through a FIFO.
mkfifo "$RUN/host.in"
"$TRIAD" --data-dir "$RUN/host-data" create --no-bot --auto-accept --json \
  <"$RUN/host.in" >"$RUN/host.out" 2>"$RUN/host.err" &
HOST_PID=$!
exec 3>"$RUN/host.in"

for _ in $(seq 60); do grep -q '"event":"ready"' "$RUN/host.out" && break; sleep 0.5; done
INVITE=$(sed -n 's/.*"invite":"\([^"]*\)".*/\1/p' "$RUN/host.out" | head -1)
[ -n "$INVITE" ] || { log "FAIL: host never printed an invite"; kill "$HOST_PID"; exit 1; }

HOST_LINE="hello from the host"
VM_LINE="hello from the vm socket"

# Once the guest has joined, the host sends its line.
(
  for _ in $(seq 600); do grep -q '"event":"joined"' "$RUN/host.out" && break; sleep 0.5; done
  sleep 2
  echo "$HOST_LINE" >&3
) &

# The socket client, retrying until the guest's serve is up.
python3 "$HERE/ws-chat.py" "127.0.0.1:$WS_PORT" "$RUN/guest-data/api-token" \
  "$INVITE" "$HOST_LINE" "$VM_LINE" >"$RUN/ws.out" 2>"$RUN/ws.err" &
WS_PID=$!

log "booting $ARCH guest with ${MEM} MB, serving on 127.0.0.1:$WS_PORT"
VM_DATA=$RUN/guest-data VM_ENV="TRIAD_AGENT_BRAIN=mock" \
  VM_HOSTFWD="tcp:127.0.0.1:$WS_PORT-:7878" \
  VM_WAIT=${VM_WAIT:-90} VM_SETTLE=${VM_SETTLE:-10} VM_TIMEOUT=${VM_TIMEOUT:-180} \
  "$HERE/run-vm.sh" "$ARCH" "$MEM" "$RUN/vm.log" serve --listen 0.0.0.0:7878 --allow-remote
QEMU_EXIT=$?

wait "$WS_PID"; WS_EXIT=$?
sleep 1
echo /quit >&3
wait "$HOST_PID"; HOST_EXIT=$?
exec 3>&-

HOST_SAW_VM=$(grep -c "\"text\":\"$VM_LINE\"" "$RUN/host.out" || true)
echo "arch=$ARCH mem=${MEM}MB ws_exit=$WS_EXIT host_saw_vm=$HOST_SAW_VM qemu_exit=$QEMU_EXIT host_exit=$HOST_EXIT"
grep -E 'MemTotal|MemAvailable|VmHWM|RssAnon|TRIAD_EXIT|Out of memory' "$RUN/vm.log" | cut -c1-120

if [ "$WS_EXIT" = 0 ] && grep -q WS_CHAT_OK "$RUN/ws.out" && [ "$HOST_SAW_VM" -ge 1 ] \
  && grep -q '^TRIAD_EXIT=0' "$RUN/vm.log"; then
  log "PASS"
else
  log "FAIL (logs in $RUN)"
  exit 1
fi
