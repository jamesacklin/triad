#!/usr/bin/env bash
# The real-model checks HARNESS-PLAN.md H7 and H8 call for, in one run:
#
#   1. H7: the ignored live tests (crates/triad/tests/bots_live.rs): the H2
#      exchange, one tool call through the MCP fixture over stdio, and the
#      planted-instruction experiment. Prints TURN lines (model time, tool
#      time, tokens) and PLANTED_RESULT.
#   2. H8: the VM guest hosts a channel bot on the real model with an HTTP
#      MCP server on this machine (scripts/vm/bot-test.sh, LIVE=1), at 64 MB
#      and, if that fails, 128 MB.
#
#   ANTHROPIC_API_KEY=... scripts/live-check.sh
#   OPENROUTER_API_KEY=... scripts/live-check.sh     # the same model, through OpenRouter
#
# Output goes to the terminal and to target/live-check.log. The key is read
# from the environment and never printed.
set -uo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
LOG=$ROOT/target/live-check.log
[ -n "${ANTHROPIC_API_KEY:-}${OPENROUTER_API_KEY:-}" ] \
  || { echo "set ANTHROPIC_API_KEY or OPENROUTER_API_KEY" >&2; exit 2; }
mkdir -p "$ROOT/target"
cd "$ROOT"
{
  echo "=== live-check $(date -Is) at $(git rev-parse --short HEAD)"
  echo "=== H7: live tests"
  cargo test -p triad --test bots_live -- --ignored --nocapture --test-threads=1 2>&1 \
    | grep -E '^test |test result|panicked|BRAIN|REPLY|TURN|CALLS|PLANTED_RESULT|assert'
  echo "=== H8: guest on the real model with an HTTP MCP server"
  cargo build -q -p triad && cargo build -q -p triad --example mcp_fixture
  for mem in 64 128; do
    if LIVE=1 TRIAD="$ROOT/target/debug/triad" scripts/vm/bot-test.sh x86_64 "$mem" 2>&1; then
      echo "H8 live: PASS at ${mem} MB"; break
    fi
    echo "H8 live: FAIL at ${mem} MB"
  done
  echo "=== done $(date -Is)"
} 2>&1 | tee "$LOG"
