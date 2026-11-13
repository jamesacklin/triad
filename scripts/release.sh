#!/usr/bin/env bash
# Build triad for this machine's target and write a signed release into a
# directory that any static HTTP server can host (see crates/triad-update).
# The web app (web/) is built first and embedded in the binary (the `web`
# feature), so it ships with every update; that needs Node and pnpm.
#
#   scripts/release.sh <version> <seq> <out-dir> [TARGET=PATH ...]
#   scripts/release.sh 0.2.0 7 ./site
#
# <seq> must be higher than every release before it. Binaries built on other
# machines (the Mac build, for one) are added as TARGET=PATH arguments; a
# manifest lists every target of a release, so publish them together.
#
# Then upload <out-dir>/blobs first and <out-dir>/manifest last.
#
# Environment:
#   RELEASE_KEY        signing key file from `triad release keygen` (required)
#   TRIAD_UPDATE_URL   compiled in as the binary's default update source
#   TRIAD_UPDATE_KEYS  compiled in as the keys it trusts; defaults to the
#                      public key of RELEASE_KEY. For a rotation, give the
#                      current and the next public key, separated by commas.
#   EXPIRES_DAYS       how long clients accept this manifest (default: 30)
set -euo pipefail

VERSION=${1:?usage: release.sh <version> <seq> <out-dir> [TARGET=PATH ...]}
SEQ=${2:?release counter}
OUT=${3:?output directory}
shift 3
: "${RELEASE_KEY:?set RELEASE_KEY to the signing key file}"

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
TRIPLE=$(rustc -vV | sed -n 's/^host: //p')

# The public key of RELEASE_KEY, from a build that needs nothing compiled in.
if [ -z "${TRIAD_UPDATE_KEYS:-}" ]; then
  cargo build --quiet --locked -p triad
  TRIAD_UPDATE_KEYS=$(target/debug/triad release pubkey "$RELEASE_KEY")
fi
export TRIAD_UPDATE_KEYS TRIAD_VERSION=$VERSION

# The web app, built fresh so the binary cannot embed a stale web/dist.
pnpm -C web install --frozen-lockfile
pnpm -C web build

case $TRIPLE in
  *-linux-gnu) export RUSTFLAGS="${RUSTFLAGS:-} -C target-feature=+crt-static" ;;
esac
cargo build --release --locked -p triad --target "$TRIPLE" --features web \
  --config 'profile.release.strip=true' \
  --config 'profile.release.lto="fat"' \
  --config 'profile.release.codegen-units=1' \
  --config 'profile.release.opt-level="s"'
BIN=target/$TRIPLE/release/triad

BINARIES=(--binary "$TRIPLE=$BIN")
for pair in "$@"; do BINARIES+=(--binary "$pair"); done
"$BIN" release publish --key "$RELEASE_KEY" --dir "$OUT" --version "$VERSION" --seq "$SEQ" \
  --expires-days "${EXPIRES_DAYS:-30}" "${BINARIES[@]}"
echo "trusted keys compiled in: $TRIAD_UPDATE_KEYS" >&2
