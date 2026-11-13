#!/usr/bin/env bash
# Runs the web app against a local `triad serve`, for development:
#
#   scripts/web-dev.sh                 # mock bot, data in demo-data/web
#   DATA_DIR=/tmp/alice scripts/web-dev.sh
#   BRAIN= scripts/web-dev.sh          # the configured model, not the mock
#
# Starts `triad serve` with --allow-origin set to the dev server's exact
# origin, prints where the token is (never the token), then runs `pnpm dev`
# in web/ on that origin. Ctrl-C stops both.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
DATA_DIR=${DATA_DIR:-$ROOT/demo-data/web}
LISTEN=${LISTEN:-127.0.0.1:7878}
WEB_PORT=${WEB_PORT:-5173}
ORIGIN="http://localhost:$WEB_PORT"
BRAIN=${BRAIN-mock}

command -v pnpm >/dev/null || { echo "pnpm is not installed (https://pnpm.io)" >&2; exit 2; }
cd "$ROOT"
cargo build -q -p triad
mkdir -p "$DATA_DIR"

if [ -n "$BRAIN" ]; then export TRIAD_AGENT_BRAIN=$BRAIN; fi
"$ROOT/target/debug/triad" --data-dir "$DATA_DIR" serve \
  --listen "$LISTEN" --allow-origin "$ORIGIN" &
SERVE=$!
trap 'kill "$SERVE" 2>/dev/null || true' EXIT INT TERM

# The token file is written on the first `serve`; wait for it.
for _ in $(seq 50); do
  [ -s "$DATA_DIR/api-token" ] && break
  kill -0 "$SERVE" 2>/dev/null || { echo "triad serve exited" >&2; exit 1; }
  sleep 0.1
done

cat <<EOF

  open      $ORIGIN
  address   ws://$LISTEN/v1
  token     $DATA_DIR/api-token   (paste its contents into the connect screen)

EOF

cd "$ROOT/web"
[ -d node_modules ] || pnpm install --frozen-lockfile
pnpm dev --host localhost --port "$WEB_PORT" --strictPort
