#!/usr/bin/env bash
# The bot in the VM (HARNESS-PLAN.md, H8): the guest creates a channel (with
# its bot, the default) on the mock brain, so it hosts the channel's bot; a triad on this
# machine joins and asks the bot. Exits 0 only if this side saw the bot's
# reply and the guest triad exited cleanly.
#
#   scripts/vm/bot-test.sh <x86_64|arm64> <mem-MB>
#
# `triad` must be on PATH for the host side (or set TRIAD). Results are kept
# in target/vm/<arch>/bot-runs/<mem>/. VM_ENV adds to the guest's bot
# environment (default: the mock brain).
#
# LIVE=1 runs the real model instead (H8's manual run): the guest's bot uses
# Anthropic with the ANTHROPIC_API_KEY of this shell (or OpenRouter, with
# OPENROUTER_API_KEY), and an HTTP MCP server
# (examples/mcp_fixture, set FIXTURE) on this machine, reached from the guest
# at QEMU's host address 10.0.2.2. The guest, which hosts the bot and so may
# use every tool, asks its own bot to make a tool call; the member on this
# machine must see the reply. It passes when the reply arrives and the
# fixture logged the call. The key is written only into the guest's
# throwaway root filesystem, which run-vm.sh deletes after the run.
# LIVE=1 LIVE_BRAIN=mock does the same with the mock brain scripted to call
# the tool: everything but the model, and no key.
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
ASK="@bot hello from the host"
FIXTURE_PID=""
if [ "${LIVE:-0}" = 1 ]; then
  if [ -z "${LIVE_BRAIN:-}" ]; then
    if [ -n "${OPENROUTER_API_KEY:-}" ]; then LIVE_BRAIN=openrouter
    elif [ -n "${ANTHROPIC_API_KEY:-}" ]; then LIVE_BRAIN=anthropic
    else log "LIVE=1 needs ANTHROPIC_API_KEY or OPENROUTER_API_KEY (or LIVE_BRAIN=mock)"; exit 2
    fi
  fi
  FIXTURE=${FIXTURE:-$ROOT/target/debug/examples/mcp_fixture}
  "$FIXTURE" --server calendar --tool create_event --log "$RUN/tool-calls.log" \
    --http 0.0.0.0:0 --allow-host 10.0.2.2 >"$RUN/fixture.out" &
  FIXTURE_PID=$!
  for _ in $(seq 40); do grep -q listening "$RUN/fixture.out" && break; sleep 0.25; done
  PORT=$(sed -n 's/.*:\([0-9]*\)\/mcp$/\1/p' "$RUN/fixture.out")
  mkdir -p "$RUN/guest-data"
  printf '{"servers":[{"name":"calendar","url":"http://10.0.2.2:%s/mcp"}]}\n' "$PORT" \
    >"$RUN/guest-data/mcp.json"
  export VM_DATA=$RUN/guest-data
  if [ "${LIVE_BRAIN:-anthropic}" = mock ]; then
    echo 'calendar => call calendar.create_event {"title": "hedge trimming"} => Added ({result})' \
      >"$RUN/guest-data/script"
    export VM_ENV="TRIAD_AGENT_BRAIN=mock
TRIAD_AGENT_MOCK_SCRIPT=/data/script"
  elif [ "$LIVE_BRAIN" = openrouter ]; then
    export VM_ENV="TRIAD_AGENT_MODEL=openrouter/anthropic/claude-opus-5.5
OPENROUTER_API_KEY=$OPENROUTER_API_KEY
RUST_LOG=warn"
  else
    export VM_ENV="TRIAD_AGENT_MODEL=anthropic/claude-opus-5-5
ANTHROPIC_API_KEY=$ANTHROPIC_API_KEY
RUST_LOG=warn"
  fi
  log "live brain: $LIVE_BRAIN"
  export VM_WAIT=${VM_WAIT_LIVE:-25} VM_SETTLE=${VM_SETTLE_LIVE:-90}
  export VM_TIMEOUT=${VM_TIMEOUT:-240}
  export VM_LINE='@bot please use the calendar tool to create an event titled "hedge trimming", then tell me it is done'
  ASK=""
else
  export VM_ENV=${VM_ENV:-TRIAD_AGENT_BRAIN=mock}
fi
log "booting $ARCH guest with ${MEM} MB, hosting #garden with its bot"
"$HERE/run-vm.sh" "$ARCH" "$MEM" "$RUN/vm.log" create --name garden --auto-accept &
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
"$TRIAD" --data-dir "$RUN/host-data" join "$INVITE" --no-bot --json \
  <"$RUN/host.in" >"$RUN/host.out" 2>"$RUN/host.err" &
HOST_PID=$!
exec 3>"$RUN/host.in"

for _ in $(seq 240); do grep -q '"event":"joined"' "$RUN/host.out" && break; sleep 0.5; done
JOINED=$(date +%s%N)
grep -q '"event":"bot_on"' "$RUN/host.out" || sleep 2
[ -n "$ASK" ] && echo "$ASK" >&3
for _ in $(seq 360); do grep -q '"bot":true' "$RUN/host.out" && break; sleep 0.25; done
REPLIED=$(date +%s%N)

wait "$VM_PID"; QEMU_EXIT=$?
echo /quit >&3
wait "$HOST_PID"; HOST_EXIT=$?
exec 3>&-
[ -n "$FIXTURE_PID" ] && kill "$FIXTURE_PID" 2>/dev/null

REPLY=$(grep '"bot":true' "$RUN/host.out" | head -1 | sed -n 's/.*"text":"\([^"]*\)".*/\1/p')
echo "arch=$ARCH mem=${MEM}MB join_ms=$(( (JOINED - START) / 1000000 )) ask_to_reply_ms=$(( (REPLIED - JOINED) / 1000000 )) qemu_exit=$QEMU_EXIT host_exit=$HOST_EXIT"
echo "bot_on: $(grep -c '"event":"bot_on"' "$RUN/host.out") reply: ${REPLY:-none}"
grep -E 'MemTotal|MemFree|MemAvailable|VmHWM|RssAnon|RssFile|TRIAD_EXIT|Out of memory' "$RUN/vm.log" | cut -c1-120

if [ "${LIVE:-0}" = 1 ]; then
  CALLS=$(cat "$RUN/tool-calls.log" 2>/dev/null | wc -l)
  echo "live: tool calls reaching the HTTP MCP server: $CALLS"
  grep '"event":"info"' "$RUN/vm.log" | cut -c1-200
  if [ -n "$REPLY" ] && [ "$CALLS" -ge 1 ] && grep -q '^TRIAD_EXIT=0' "$RUN/vm.log"; then
    log "PASS"; exit 0
  fi
  log "FAIL (logs in $RUN)"; exit 1
fi
if [ "$REPLY" = "echo: @bot hello from the host" ] && grep -q '^TRIAD_EXIT=0' "$RUN/vm.log"; then
  log "PASS"
else
  log "FAIL (logs in $RUN)"
  exit 1
fi
