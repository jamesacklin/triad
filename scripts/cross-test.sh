#!/usr/bin/env bash
# Cross-machine acceptance run (steps 1-9 of "Acceptance run" in the spec).
#
# Runs the creator here and the joiner on REMOTE over SSH, both with fresh
# data directories and --json output. `triad` must be on PATH on both sides.
#
#   scripts/cross-test.sh user@linux-box            # this machine creates
#   LOCAL_TAG=linux REMOTE_TAG=mac scripts/cross-test.sh user@mac
#   scripts/cross-test.sh local                     # both on this machine (smoke test)
#
# Works with the bash 3.2 that ships with macOS (no coproc, no jq).
set -euo pipefail

REMOTE=${1:?usage: cross-test.sh <ssh-host|local>}
LOCAL_TAG=${LOCAL_TAG:-mac}
REMOTE_TAG=${REMOTE_TAG:-linux}
TIMEOUT=${TIMEOUT:-180}
TRIAD=${TRIAD:-triad}
REMOTE_TRIAD=${REMOTE_TRIAD:-triad}

WORK=$(mktemp -d)
LOCAL_DIR="$WORK/a"

remote() {
  if [ "$REMOTE" = local ]; then
    bash -c "$*"
  else
    ssh -T -o BatchMode=yes "$REMOTE" "$*"
  fi
}

REMOTE_DIR=$(remote 'mktemp -d')/b

log() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*" >&2; }
fail() { log "FAIL: $*"; exit 1; }

# --- process plumbing: stdin and stdout of each side go through FIFOs ---
mkfifo "$WORK/a.in" "$WORK/a.out" "$WORK/b.in" "$WORK/b.out"
A_PID=""; B_PID=""

start_a() { # args...
  "$TRIAD" --data-dir "$LOCAL_DIR" "$@" <"$WORK/a.in" >"$WORK/a.out" 2>>"$WORK/a.err" &
  A_PID=$!
  exec 3>"$WORK/a.in" 4<"$WORK/a.out"
}

start_b() { # args...
  remote "$REMOTE_TRIAD --data-dir '$REMOTE_DIR' $*" <"$WORK/b.in" >"$WORK/b.out" 2>>"$WORK/b.err" &
  B_PID=$!
  exec 5>"$WORK/b.in" 6<"$WORK/b.out"
}

stop_a() { echo /quit >&3 || true; exec 3>&- ; drain_events 4 a; wait "$A_PID" || fail "creator exited unsuccessfully"; exec 4<&- ; A_PID=""; }
stop_b() { echo /quit >&5 || true; exec 5>&- ; drain_events 6 b; wait "$B_PID" || fail "joiner exited unsuccessfully"; exec 6<&- ; B_PID=""; }

cleanup() {
  [ -n "$A_PID" ] && kill "$A_PID" 2>/dev/null || true
  [ -n "$B_PID" ] && kill "$B_PID" 2>/dev/null || true
  log "logs and data: $WORK (remote data: $REMOTE:$REMOTE_DIR)"
}

trap 'cleanup' EXIT

# Consume remaining output while the process shuts down, including duplicates.
drain_events() {
  local fd=$1 name=$2 line status
  while true; do
    if IFS= read -r -t "$TIMEOUT" line <&"$fd"; then
      printf '%s\n' "$line" >>"$WORK/$name.events"
    else
      status=$?
      [ "$status" -eq 1 ] || fail "$name: timed out draining output during shutdown"
      return 0
    fi
  done
}

# wait_for <fd> <name> <grep-pattern>: reads JSON lines until one matches; prints it.
wait_for() {
  local fd=$1 name=$2 pattern=$3 line deadline=$((SECONDS + TIMEOUT))
  while [ $SECONDS -lt $deadline ]; do
    if IFS= read -r -t 5 line <&"$fd"; then
      printf '%s\n' "$line" >>"$WORK/$name.events"
      if printf '%s' "$line" | grep -q -- "$pattern"; then
        printf '%s\n' "$line"
        return 0
      fi
    fi
  done
  fail "$name: timed out waiting for $pattern"
}

field() { # field <json-line> <key>: crude extraction of a string or number value
  printf '%s' "$1" | sed -n "s/.*\"$2\":\"\{0,1\}\([^\",}]*\).*/\1/p"
}

# collect_texts <fd> <name> <count> <prefix>: waits for <count> foreign messages starting with <prefix>.
collect_texts() {
  local fd=$1 name=$2 count=$3 prefix=$4 got=0 line out=""
  while [ $got -lt "$count" ]; do
    line=$(wait_for "$fd" "$name" "\"event\":\"message\".*\"mine\":false\|\"mine\":false.*\"event\":\"message\"")
    out="$out $(field "$line" text)"
    got=$((got + 1))
  done
  printf '%s\n' "$out" | sed 's/^ //'
  case "$out" in *"$prefix"*) ;; *) fail "$name: unexpected messages: $out";; esac
}

expect_eq() { [ "$1" = "$2" ] || fail "$3: expected '$2', got '$1'"; log "ok: $3"; }

seq_names() { local tag=$1 from=$2 to=$3 i; for i in $(seq "$from" "$to"); do printf '%s-%02d ' "$tag" "$i"; done | sed 's/ $//'; }

# --- 1. creator ---
T0=$SECONDS
start_a create --auto-accept --json -v
READY=$(wait_for 4 a '"event":"ready"')
INVITE=$(field "$READY" invite)
[ -n "$INVITE" ] || fail "no invite in: $READY"
log "1. created group, invite ${INVITE:0:24}..."

# --- 2. joiner ---
start_b join "$INVITE" --json -v
wait_for 6 b '"event":"ready"' >/dev/null
JB=$(wait_for 6 b '"event":"joined"')
JA=$(wait_for 4 a '"event":"joined"')
log "2. joined after $((SECONDS - T0))s"

# --- 3. two members at epoch 1 ---
expect_eq "$(field "$JA" members)/$(field "$JA" epoch)" "2/1" "3. creator sees 2 members at epoch 1"
expect_eq "$(field "$JB" members)/$(field "$JB" epoch)" "2/1" "3. joiner sees 2 members at epoch 1"

# --- 4. 10 messages each way ---
for m in $(seq_names "$LOCAL_TAG" 1 10); do echo "$m" >&3; done
for m in $(seq_names "$REMOTE_TAG" 1 10); do echo "$m" >&5; done
expect_eq "$(collect_texts 6 b 10 "$LOCAL_TAG")" "$(seq_names "$LOCAL_TAG" 1 10)" "4. joiner got creator's 10 in order"
expect_eq "$(collect_texts 4 a 10 "$REMOTE_TAG")" "$(seq_names "$REMOTE_TAG" 1 10)" "4. creator got joiner's 10 in order"

# --- 5. history shows 20 on both ---
expect_eq "$("$TRIAD" --data-dir "$LOCAL_DIR" history | wc -l | tr -d ' ')" "20" "5. creator history has 20"
expect_eq "$(remote "$REMOTE_TRIAD --data-dir '$REMOTE_DIR' history" | wc -l | tr -d ' ')" "20" "5. joiner history has 20"

# --- 6. joiner quits, creator sends 5 ---
stop_b
for m in $(seq_names "$LOCAL_TAG" 11 15); do echo "$m" >&3; done
for m in $(seq_names "$LOCAL_TAG" 11 15); do wait_for 4 a "\"text\":\"$m\"" >/dev/null; done
log "6. joiner offline, creator sent 5"

# --- 7. joiner returns and catches up on exactly 5 ---
T1=$SECONDS
start_b chat --json -v
expect_eq "$(collect_texts 6 b 5 "$LOCAL_TAG")" "$(seq_names "$LOCAL_TAG" 11 15)" "7. joiner caught up on exactly 5"
log "7. catch-up took $((SECONDS - T1))s"
grep -q '"event":"sync_started"' "$WORK/b.events" && log "7. sync observed" || log "7. WARNING: no sync_started event seen"

# --- 8. quit both, inspect for plaintext ---
sleep 5
stop_b
stop_a
EXTRA=$(grep -c "\"mine\":false" "$WORK/b.events" || true)
for needle in "$LOCAL_TAG-01" "$REMOTE_TAG-01"; do
  "$TRIAD" --data-dir "$LOCAL_DIR" inspect --grep "$needle" >/dev/null || fail "8. plaintext $needle in creator's p2panda.sqlite"
  remote "$REMOTE_TRIAD --data-dir '$REMOTE_DIR' inspect --grep '$needle'" >/dev/null || fail "8. plaintext $needle in joiner's p2panda.sqlite"
done
log "8. no plaintext in either p2panda.sqlite"

# --- 9. status: same epoch, same members, zero failed ---
SA=$("$TRIAD" --data-dir "$LOCAL_DIR" status)
SB=$(remote "$REMOTE_TRIAD --data-dir '$REMOTE_DIR' status")
printf '%s\n' "$SA" | grep -q 'failed=0' || fail "9. creator has failed operations"
printf '%s\n' "$SB" | grep -q 'failed=0' || fail "9. joiner has failed operations"
expect_eq "$(printf '%s\n' "$SA" | grep '^epoch')" "$(printf '%s\n' "$SB" | grep '^epoch')" "9. same epoch"
members_of() { printf '%s\n' "$1" | sed -n '/^members/,/^ops/p' | grep -v '^ops'; }
expect_eq "$(members_of "$SA")" "$(members_of "$SB")" "9. same members"
expect_eq "$EXTRA" "15" "no duplicate deliveries to the joiner (10 + 5 foreign messages)"
log "PASS ($((SECONDS - T0))s total)"
