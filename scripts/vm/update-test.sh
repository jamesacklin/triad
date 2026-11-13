#!/usr/bin/env bash
# Over-the-air update inside the guest, end to end. A release is published
# to a directory served over plain HTTP on this machine, and the guest, with
# its init as supervisor and a data disk that outlives the boot, has to:
#
#   boot 1  start the binary in its image, download and check the next
#           version, restart into it, and confirm it after it stays up;
#   boot 2  start that version straight from the data disk, install a
#           release whose binary exits at once, go back to the version
#           before, and not install the bad release again.
#
#   scripts/vm/update-test.sh <x86_64|arm64> <mem-MB>
#
# Needs the output of build.sh for the architecture and a second triad
# version next to it (the VM_TRIAD_NAME example in build.sh). Results are
# kept in target/vm/<arch>/update-runs/<mem>/.
#
# Environment overrides:
#   TRIAD          a triad for this machine, to sign the releases
#                  (default: the guest binary if it runs here, else `triad`)
#   NEXT_VERSION   what triad-next prints for --version (default: 0.1.1-next)
#   VM_CC          C compiler for the guest, for the bad binary
#   VM_HTTP_PORT   port for the release server (default: 18432)
#   VM_SLOW        multiplies every wait; default 1, or 4 for arm64
set -uo pipefail

ARCH=${1:?usage: update-test.sh <x86_64|arm64> <mem-MB>}
MEM=${2:?memory in MB}
NEXT_VERSION=${NEXT_VERSION:-0.1.1-next}
PORT=${VM_HTTP_PORT:-18432}

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
A=${VM_OUT:-$ROOT/target/vm}/$ARCH
RUN=$A/update-runs/$MEM

case $ARCH in
  x86_64) TRIPLE=x86_64-unknown-linux-gnu; CC=${VM_CC:-gcc}; SLOW=${VM_SLOW:-1} ;;
  arm64) TRIPLE=aarch64-unknown-linux-gnu; CC=${VM_CC:-aarch64-linux-gnu-gcc}; SLOW=${VM_SLOW:-4} ;;
  *) echo "unknown architecture: $ARCH" >&2; exit 2 ;;
esac
if [ -z "${TRIAD:-}" ]; then
  if [ "$(uname -s)-$(uname -m)" = "Linux-${TRIPLE%%-*}" ]; then TRIAD=$A/triad; else TRIAD=triad; fi
fi
for f in triad triad-next kernel init; do
  [ -f "$A/$f" ] || { echo "missing $A/$f; see build.sh" >&2; exit 1; }
done

log() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*" >&2; }
FAILED=0
# expect <log> <what> <fixed string>
expect() {
  if grep -qF -- "$3" "$1"; then log "  ok: $2"; else log "  MISSING: $2 ($3)"; FAILED=1; fi
}

rm -rf "$RUN"; mkdir -p "$RUN/site"
PUBLIC=$("$TRIAD" release keygen "$RUN/release.key" 2>/dev/null) \
  || { log "FAIL: $TRIAD has no 'release keygen'"; exit 1; }
mkfs.ext4 -q -L data "$RUN/data.ext4" 128M

# A release that passes the updater's checks and then fails to stay up.
cat >"$RUN/bad.c" <<'EOF'
#include <stdio.h>
#include <string.h>
int main(int argc, char **argv) {
    if (argc > 1 && !strcmp(argv[1], "--version")) { puts("triad 9.9.9"); return 0; }
    return 1;
}
EOF
"$CC" -Os -static -s -o "$RUN/bad" "$RUN/bad.c" || { log "FAIL: could not compile the bad binary"; exit 1; }

python3 -m http.server "$PORT" --bind 127.0.0.1 -d "$RUN/site" >"$RUN/http.log" 2>&1 &
HTTP_PID=$!
trap 'kill "$HTTP_PID" 2>/dev/null' EXIT

# boot <log> <first check> <interval> <lifetime> <triad args...>
boot() {
  local out=$1 first=$2 interval=$3 life=$4
  shift 4
  # 10.0.2.2 is this machine, as QEMU's user network shows it to the guest.
  VM_ENV="TRIAD_SUPERVISE=1
TRIAD_UPDATE_URL=http://10.0.2.2:$PORT
TRIAD_UPDATE_KEYS=$PUBLIC
TRIAD_UPDATE_FIRST=$((first * SLOW))
TRIAD_UPDATE_INTERVAL=$((interval * SLOW))
TRIAD_CONFIRM_SECS=$((15 * SLOW))
TRIAD_LIFETIME=$((life * SLOW))" \
  VM_DATA_DISK="$RUN/data.ext4" VM_RESUME="chat --no-bot" VM_TIMEOUT=$(((life + 60) * SLOW)) \
    "$HERE/run-vm.sh" "$ARCH" "$MEM" "$out" "$@"
}

log "release 1: triad $NEXT_VERSION"
"$TRIAD" release publish --key "$RUN/release.key" --dir "$RUN/site" \
  --version "$NEXT_VERSION" --seq 1 --binary "$TRIPLE=$A/triad-next" >/dev/null || exit 1
log "boot 1: $ARCH guest with ${MEM} MB"
boot "$RUN/boot1.log" 15 3600 75 create --name garden --no-bot
B=$RUN/boot1.log
expect "$B" "starts the binary in the image" "SUPERVISOR start slot=factory trial=0"
expect "$B" "downloads and installs the release" "update: installed triad $NEXT_VERSION in slot a"
expect "$B" "restarts into it, on trial" "SUPERVISOR start slot=a trial=1"
expect "$B" "the new version is what runs" "triad $NEXT_VERSION"
expect "$B" "confirms it" "SUPERVISOR confirmed slot=a"
expect "$B" "it shuts down cleanly" "SUPERVISOR triad exit=0 slot=a"

log "release 2: a binary that exits at once"
"$TRIAD" release publish --key "$RUN/release.key" --dir "$RUN/site" \
  --version 9.9.9 --seq 2 --binary "$TRIPLE=$RUN/bad" >/dev/null || exit 1
log "boot 2: same data disk"
boot "$RUN/boot2.log" 10 20 75 chat --no-bot
B=$RUN/boot2.log
expect "$B" "starts the installed version from the data disk" "SUPERVISOR start slot=a trial=0"
expect "$B" "installs the bad release" "update: installed triad 9.9.9 in slot b"
expect "$B" "tries it" "SUPERVISOR start slot=b trial=1"
expect "$B" "goes back when it exits" "SUPERVISOR rollback from=b to=a"
expect "$B" "does not install it again" "update: triad 9.9.9 failed here before"
expect "$B" "ends on the good version" "SUPERVISOR triad exit=0 slot=a"
[ "$(grep -c 'update: installed' "$B")" = 1 ] || { log "  MISSING: exactly one install in boot 2"; FAILED=1; }

echo "arch=$ARCH mem=${MEM}MB"
grep -hE 'MemTotal|MemFree|MemAvailable|VmHWM|RssAnon|Out of memory' "$RUN/boot1.log" | cut -c1-120
if [ "$FAILED" = 0 ]; then
  log "PASS"
else
  log "FAIL (logs in $RUN)"
  exit 1
fi
