#!/usr/bin/env bash
# The bot in the VM (HARNESS-PLAN.md, H8): the guest creates a channel with
# --agent on the mock brain, so it hosts the channel's bot; a triad on this
# machine joins and asks the bot. Exits 0 only if this side saw the bot's
# reply and the guest triad exited cleanly.
#
#   scripts/vm/bot-test.sh <x86_64|arm64> <mem-MB>
#
# `triad` must be on PATH for the host side (or set TRIAD). Results are kept
# in target/vm/<arch>/bot-runs/<mem>/. VM_ENV adds to the guest's bot
# environment (default: the mock brain).
set -uo pipefail

ARCH=${1:?usage: bot-test.sh <x86_64|arm64> <mem-MB>}
MEM=${2:?memory in MB}
TRIAD=${TRIAD:-triad}

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
RUN=${VM_OUT:-$ROOT/target/vm}/$ARCH/bot-runs/$MEM
rm -rf "$RUN"; mkdir -p "$RUN"

log() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*" >&2; }

# The guest keeps running for VM_WAIT + VM_SETTLE seconds; leave the host
# time to join and get its answer.
export VM_WAIT=${VM_WAIT:-60} VM_SETTLE=${VM_SETTLE:-20}
export VM_ENV=${VM_ENV:-TRIAD_AGENT_BRAIN=mock}
log "booting $ARCH guest with ${MEM} MB, hosting #garden with its bot"
"$HERE/run-vm.sh" "$ARCH" "$MEM" "$RUN/vm.log" create --name garden --agent --auto-accept &
VM_PID=$!

INVITE=""
for _ in $(seq 240); do
  INVITE=$(sed -n 's/.*"event":"ready".*"invite":"\([^"]*\)".*/\1/p' "$RUN/vm.log" 2>/dev/null | head -1)
  [ -z "$INVITE" ] && INVITE=$(grep '"event":"ready"' "$RUN/vm.log" 2>/dev/null | sed -n 's/.*"invite":"\(triad1-[0-9a-f]*\)".*/\1/p' | head -1)
  [ -n "$INVITE" ] && break
  sleep 0.5
done
[ -n "$INVITE" ] || { log "FAIL: the guest never printed an invite"; wait "$VM_PID"; exit 1; }
log "guest ready; joining"

mkfifo "$RUN/host.in"
START=$(date +%s%N)
"$TRIAD" --data-dir "$RUN/host-data" join "$INVITE" --json \
  <"$RUN/host.in" >"$RUN/host.out" 2>"$RUN/host.err" &
HOST_PID=$!
exec 3>"$RUN/host.in"

for _ in $(seq 240); do grep -q '"event":"joined"' "$RUN/host.out" && break; sleep 0.5; done
JOINED=$(date +%s%N)
grep -q '"event":"bot_on"' "$RUN/host.out" || sleep 2
echo "@bot hello from the host" >&3
for _ in $(seq 120); do grep -q '"bot":true' "$RUN/host.out" && break; sleep 0.25; done
REPLIED=$(date +%s%N)

wait "$VM_PID"; QEMU_EXIT=$?
echo /quit >&3
wait "$HOST_PID"; HOST_EXIT=$?
exec 3>&-

REPLY=$(grep '"bot":true' "$RUN/host.out" | head -1 | sed -n 's/.*"text":"\([^"]*\)".*/\1/p')
echo "arch=$ARCH mem=${MEM}MB join_ms=$(( (JOINED - START) / 1000000 )) ask_to_reply_ms=$(( (REPLIED - JOINED) / 1000000 )) qemu_exit=$QEMU_EXIT host_exit=$HOST_EXIT"
echo "bot_on: $(grep -c '"event":"bot_on"' "$RUN/host.out") reply: ${REPLY:-none}"
grep -E 'MemTotal|MemFree|MemAvailable|VmHWM|RssAnon|RssFile|TRIAD_EXIT|Out of memory' "$RUN/vm.log" | cut -c1-120

if [ "$REPLY" = "echo: @bot hello from the host" ] && grep -q '^TRIAD_EXIT=0' "$RUN/vm.log"; then
  log "PASS"
else
  log "FAIL (logs in $RUN)"
  exit 1
fi
