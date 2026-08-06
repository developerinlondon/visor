#!/usr/bin/env bash
# Build the visor guest kernel from source.
#
# Supports x86_64 (vmlinux) and aarch64 (Image) architectures.
# Auto-detects host architecture, or override with VISOR_KERNEL_ARCH.
#
# This script resolves config fragments into a full .config, then compiles
# the kernel. Config fragments live in config/fragments/{arch}/.
#
# For Rust support (CONFIG_RUST=y), also requires: rustc, bindgen, rust-src, llvm, lld
#
# Prerequisites: gcc, make, bc, flex, bison, libelf-dev, libssl-dev
# Usage:
#   ./crates/visor-kernel/scripts/build-kernel.sh              # build for host arch
#   ./crates/visor-kernel/scripts/build-kernel.sh /tmp/output  # custom output path
#
# Environment:
#   VISOR_KERNEL_ARCH=aarch64|x86_64  # override auto-detected architecture
#
# To update the config lockfile without building:
#   ./crates/visor-kernel/scripts/resolve-config.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CRATE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WORKSPACE_ROOT="$(cd "$CRATE_DIR/../.." && pwd)"

KERNEL_REPO="https://github.com/torvalds/linux.git"
KERNEL_TAG="v7.0-rc1"
BUILD_DIR="/tmp/linux-visor-build"
INSTALL_DIR="${1:-/var/lib/visor/kernel}"

# ── Detect architecture ──────────────────────────────────────
HOST_ARCH="$(uname -m)"
case "${VISOR_KERNEL_ARCH:-$HOST_ARCH}" in
  aarch64|arm64)
    KARCH="arm64"
    KERNEL_NAME="Image-aarch64"
    KERNEL_TARGET="Image"
    KERNEL_OUTPUT="arch/arm64/boot/Image"
    MAKE_EXTRA=""
    ;;
  x86_64|amd64)
    KARCH="x86_64"
    KERNEL_NAME="vmlinux-x86_64"
    KERNEL_TARGET="vmlinux"
    KERNEL_OUTPUT="vmlinux"
    MAKE_EXTRA=""
    ;;
  *)
    echo "ERROR: unsupported architecture: ${VISOR_KERNEL_ARCH:-$HOST_ARCH}"
    echo "       Supported: x86_64, aarch64"
    exit 1
    ;;
esac

FRAGMENTS_DIR="$CRATE_DIR/config/fragments/$KARCH"
if [ "$KARCH" = "x86_64" ]; then
  CONFIG_FILE="$CRATE_DIR/config/visor-kernel.config"
else
  CONFIG_FILE="$CRATE_DIR/config/visor-kernel-${KARCH}.config"
fi

echo "==> Architecture: $KARCH"
echo "==> Kernel output: $KERNEL_NAME"
echo "==> Fragments: $FRAGMENTS_DIR/"

# ── Extract visor version from workspace Cargo.toml ─────────
VISOR_VERSION=$(grep '^version' "$WORKSPACE_ROOT/Cargo.toml" | head -1 | sed 's/.*"\(.*\)"/\1/')
echo "==> Visor version: $VISOR_VERSION"

# ── Clone kernel source ──────────────────────────────────────
echo "==> Cloning kernel source (shallow)..."
if [ -d "$BUILD_DIR" ]; then
  echo "    Using existing source at $BUILD_DIR"
else
  git clone --depth 1 --branch "$KERNEL_TAG" "$KERNEL_REPO" "$BUILD_DIR"
fi

# ── Resolve config ────────────────────────────────────────────
# If fragments exist for this arch, re-resolve to catch any changes.
# Otherwise fall back to the committed lockfile.
if [ -d "$FRAGMENTS_DIR" ] && [ -f "$FRAGMENTS_DIR/base.config" ]; then
  echo "==> Resolving config from fragments..."
  "$SCRIPT_DIR/resolve-config.sh" "$BUILD_DIR"
fi

if [ ! -f "$CONFIG_FILE" ]; then
  echo "ERROR: kernel config not found at $CONFIG_FILE"
  echo "       Run: ./crates/visor-kernel/scripts/resolve-config.sh"
  exit 1
fi

# ── Apply config ──────────────────────────────────────────────
echo "==> Applying visor kernel config..."
cp "$CONFIG_FILE" "$BUILD_DIR/.config"
# shellcheck disable=SC2086
make -C "$BUILD_DIR" $MAKE_EXTRA ARCH="$KARCH" olddefconfig

# ── Build ─────────────────────────────────────────────────────
NPROC=$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)
echo "==> Building kernel ($NPROC threads)..."
# shellcheck disable=SC2086
make -C "$BUILD_DIR" $MAKE_EXTRA ARCH="$KARCH" \
  LOCALVERSION="-$VISOR_VERSION" \
  KBUILD_BUILD_USER=visor KBUILD_BUILD_HOST=visor.rs \
  "$KERNEL_TARGET" -j"$NPROC"

# ── Strip debug symbols (x86_64 vmlinux only) ────────────────
# The ELF vmlinux includes ~15-20MB of debug/symbol sections that
# aren't needed at runtime. Stripping reduces it from ~30MB to ~10MB.
# ARM64 Image is already a raw binary with no debug sections.
if [ "$KARCH" = "x86_64" ]; then
  echo "==> Stripping debug symbols from vmlinux..."
  UNSTRIPPED_SIZE=$(stat --format=%s "$BUILD_DIR/$KERNEL_OUTPUT" 2>/dev/null || stat -f%z "$BUILD_DIR/$KERNEL_OUTPUT")
  strip --strip-debug "$BUILD_DIR/$KERNEL_OUTPUT"
  STRIPPED_SIZE=$(stat --format=%s "$BUILD_DIR/$KERNEL_OUTPUT" 2>/dev/null || stat -f%z "$BUILD_DIR/$KERNEL_OUTPUT")
  echo "    ${UNSTRIPPED_SIZE} → ${STRIPPED_SIZE} bytes"
fi

# ── Install ───────────────────────────────────────────────────
echo "==> Installing kernel..."
mkdir -p "$INSTALL_DIR"
cp "$BUILD_DIR/$KERNEL_OUTPUT" "$INSTALL_DIR/$KERNEL_NAME"

SIZE=$(stat --format=%s "$INSTALL_DIR/$KERNEL_NAME" 2>/dev/null || stat -f%z "$INSTALL_DIR/$KERNEL_NAME")
echo "==> Done: $INSTALL_DIR/$KERNEL_NAME ($SIZE bytes)"
