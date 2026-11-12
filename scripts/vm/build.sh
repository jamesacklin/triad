#!/usr/bin/env bash
# Build everything the test guest needs for one architecture:
# a static triad binary, a minimal Linux kernel and the guest init.
#
#   scripts/vm/build.sh x86_64
#   scripts/vm/build.sh arm64
#
# Output goes to target/vm/<arch>/ (kernel image, triad, init).
#
# Environment overrides:
#   VM_OUT             output directory (default: <repo>/target/vm)
#   VM_CC              C compiler and linker for the guest
#                      (default: gcc for x86_64, aarch64-linux-gnu-gcc for arm64)
#   CROSS_COMPILE      kernel cross prefix for arm64 (default: aarch64-linux-gnu-)
#   KERNEL_MAKE_ARGS   extra arguments for the kernel `make`
#   VM_KERNEL=skip     rebuild only triad and init; keep the existing kernel
set -euo pipefail

ARCH=${1:?usage: build.sh <x86_64|arm64>}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)

KVER=6.18.63
KSHA=81764a545072d160748beff4d9de2e7d95aa9f16e8261110288f067a2cc61d71
KURL=https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-$KVER.tar.xz

case $ARCH in
  x86_64)
    TRIPLE=x86_64-unknown-linux-gnu
    CC=${VM_CC:-gcc}
    AR=ar
    KMAKE=(ARCH=x86_64)
    KTARGET=bzImage
    KIMG=arch/x86/boot/bzImage
    ;;
  arm64)
    TRIPLE=aarch64-unknown-linux-gnu
    CROSS_COMPILE=${CROSS_COMPILE:-aarch64-linux-gnu-}
    CC=${VM_CC:-${CROSS_COMPILE}gcc}
    AR=${CROSS_COMPILE}ar
    KMAKE=(ARCH=arm64 "CROSS_COMPILE=$CROSS_COMPILE")
    KTARGET=Image
    KIMG=arch/arm64/boot/Image
    ;;
  *) echo "unknown architecture: $ARCH" >&2; exit 2 ;;
esac

OUT=${VM_OUT:-$ROOT/target/vm}
A=$OUT/$ARCH
mkdir -p "$A" "$OUT/dl"

log() { printf '[build %s] %s\n' "$ARCH" "$*" >&2; }

# --- triad: static, stripped, optimised for size ---
log "building triad for $TRIPLE"
T_UPPER=$(echo "$TRIPLE" | tr 'a-z-' 'A-Z_')
T_LOWER=${TRIPLE//-/_}
(
  cd "$ROOT"
  env \
    "CARGO_TARGET_${T_UPPER}_RUSTFLAGS=-C target-feature=+crt-static" \
    "CARGO_TARGET_${T_UPPER}_LINKER=$CC" \
    "CC_${T_LOWER}=$CC" \
    "AR_${T_LOWER}=$AR" \
    cargo build --release --locked -p triad --target "$TRIPLE" \
      --config 'profile.release.strip=true' \
      --config 'profile.release.lto="fat"' \
      --config 'profile.release.codegen-units=1' \
      --config 'profile.release.opt-level="s"'
)
cp "$ROOT/target/$TRIPLE/release/triad" "$A/triad"

# --- guest init ---
log "building init"
"$CC" -Os -static -s -o "$A/init" "$HERE/init.c"

if [ "${VM_KERNEL:-}" = skip ]; then
  [ -f "$A/kernel" ] || { echo "VM_KERNEL=skip, but $A/kernel does not exist" >&2; exit 1; }
  log "keeping the existing kernel"
  ls -l "$A/kernel" "$A/triad" "$A/init" >&2
  exit 0
fi

# --- kernel ---
TARBALL=$OUT/dl/linux-$KVER.tar.xz
if [ ! -f "$TARBALL" ]; then
  log "downloading $KURL"
  curl -fsSL -o "$TARBALL.part" "$KURL"
  mv "$TARBALL.part" "$TARBALL"
fi
echo "$KSHA  $TARBALL" | sha256sum -c - >&2

# One source tree per architecture: the kernel refuses to mix them.
SRC=$A/linux-$KVER
if [ ! -d "$SRC" ]; then
  log "unpacking kernel source"
  tar -xf "$TARBALL" -C "$A"
fi

log "configuring kernel"
FRAGMENTS=("$HERE/kernel-common.config" "$HERE/kernel-$ARCH.config")
(
  cd "$SRC"
  # shellcheck disable=SC2086
  make "${KMAKE[@]}" ${KERNEL_MAKE_ARGS:-} tinyconfig >/dev/null
  ./scripts/kconfig/merge_config.sh -m .config "${FRAGMENTS[@]}" >/dev/null
  # shellcheck disable=SC2086
  make "${KMAKE[@]}" ${KERNEL_MAKE_ARGS:-} olddefconfig >/dev/null
  # Fail if the kernel dropped an option we asked for.
  missing=$(grep -hE '^CONFIG_[A-Z0-9_]+=y$' "${FRAGMENTS[@]}" | grep -vxFf .config || true)
  if [ -n "$missing" ]; then
    echo "kernel config is missing requested options:" >&2
    echo "$missing" >&2
    exit 1
  fi
  log "compiling kernel"
  # shellcheck disable=SC2086
  make "${KMAKE[@]}" ${KERNEL_MAKE_ARGS:-} -j"$(nproc)" "$KTARGET" >"$A/kernel-build.log" 2>&1 \
    || { tail -20 "$A/kernel-build.log" >&2; exit 1; }
)
cp "$SRC/$KIMG" "$A/kernel"

log "done"
ls -l "$A/kernel" "$A/triad" "$A/init" >&2
