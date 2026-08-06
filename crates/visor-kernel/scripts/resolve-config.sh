#!/usr/bin/env bash
# Resolve kernel config fragments into a full .config lockfile.
#
# Merges the fragment files in config/fragments/{arch}/ into a single resolved
# config lockfile using the kernel's own merge_config.sh + olddefconfig.
#
# This is the "Cargo.toml → Cargo.lock" step for the kernel config.
# The fragments are the source of truth. The resolved config is the lockfile.
#
# Supports x86_64 and aarch64 architectures. Auto-detects host architecture,
# or override with VISOR_KERNEL_ARCH.
#
# Prerequisites: kernel source tree (cloned by build-kernel.sh)
#
# Usage:
#   ./crates/visor-kernel/scripts/resolve-config.sh                          # uses /tmp/linux-visor-build
#   ./crates/visor-kernel/scripts/resolve-config.sh /path/to/linux-source    # custom source dir
#
# Environment:
#   VISOR_KERNEL_ARCH=aarch64|x86_64  # override auto-detected architecture

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CRATE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

KERNEL_REPO="https://github.com/torvalds/linux.git"
KERNEL_TAG="v7.0-rc1"
BUILD_DIR="${1:-/tmp/linux-visor-build}"

# ── Detect architecture ──────────────────────────────────────
HOST_ARCH="$(uname -m)"
case "${VISOR_KERNEL_ARCH:-$HOST_ARCH}" in
  aarch64|arm64)
    KARCH="arm64"
    ;;
  x86_64|amd64)
    KARCH="x86_64"
    ;;
  *)
    echo "ERROR: unsupported architecture: ${VISOR_KERNEL_ARCH:-$HOST_ARCH}"
    echo "       Supported: x86_64, aarch64"
    exit 1
    ;;
esac

FRAGMENTS_DIR="$CRATE_DIR/config/fragments/$KARCH"
if [ "$KARCH" = "x86_64" ]; then
  OUTPUT_FILE="$CRATE_DIR/config/visor-kernel.config"
else
  OUTPUT_FILE="$CRATE_DIR/config/visor-kernel-${KARCH}.config"
fi

echo "==> Architecture: $KARCH"
echo "==> Fragments: $FRAGMENTS_DIR/"
echo "==> Output: $OUTPUT_FILE"

# ── Validate fragments exist ─────────────────────────────────
FRAGMENT_FILES=(
  "$FRAGMENTS_DIR/base.config"
  "$FRAGMENTS_DIR/security.config"
  "$FRAGMENTS_DIR/devices.config"
  "$FRAGMENTS_DIR/perf.config"
  "$FRAGMENTS_DIR/rust.config"
  "$FRAGMENTS_DIR/intentional.config"
)

for f in "${FRAGMENT_FILES[@]}"; do
  if [ ! -f "$f" ]; then
    echo "ERROR: missing fragment: $f"
    exit 1
  fi
done

# ── Ensure kernel source is available ─────────────────────────
if [ ! -d "$BUILD_DIR" ]; then
  echo "==> Cloning kernel source (shallow)..."
  git clone --depth 1 --branch "$KERNEL_TAG" "$KERNEL_REPO" "$BUILD_DIR"
fi

MERGE_SCRIPT="$BUILD_DIR/scripts/kconfig/merge_config.sh"
if [ ! -f "$MERGE_SCRIPT" ]; then
  echo "ERROR: kernel merge_config.sh not found at $MERGE_SCRIPT"
  echo "       Is $BUILD_DIR a valid kernel source tree?"
  exit 1
fi

# ── Merge fragments ──────────────────────────────────────────
echo "==> Merging $(echo "${FRAGMENT_FILES[@]}" | wc -w) config fragments..."
echo "    Fragments:"
for f in "${FRAGMENT_FILES[@]}"; do
  lines=$(grep -c '^[^#]' "$f" 2>/dev/null || echo 0)
  echo "      $(basename "$f") ($lines options)"
done

# merge_config.sh uses -O to set output directory
# It creates .config in the kernel source tree
cd "$BUILD_DIR"
KCONFIG_CONFIG="$BUILD_DIR/.config" \
  "$MERGE_SCRIPT" -m "${FRAGMENT_FILES[@]}" > /dev/null 2>&1

# ── Resolve all defaults ─────────────────────────────────────
echo "==> Running olddefconfig (resolving ~3000 defaults)..."
make -C "$BUILD_DIR" ARCH="$KARCH" olddefconfig > /dev/null 2>&1

# ── Copy result to lockfile ──────────────────────────────────
cp "$BUILD_DIR/.config" "$OUTPUT_FILE"

TOTAL_LINES=$(wc -l < "$OUTPUT_FILE")
EXPLICIT=$(cat "${FRAGMENT_FILES[@]}" | grep -c '^[^#]' || echo 0)
echo "==> Resolved: $EXPLICIT fragment options → $TOTAL_LINES total lines"
echo "    Output: $OUTPUT_FILE"
echo ""
echo "    Review changes with: git diff $OUTPUT_FILE"
echo "    Then commit both fragments/ and the lockfile together."
