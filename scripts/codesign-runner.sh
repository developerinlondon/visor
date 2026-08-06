#!/usr/bin/env bash
# Cargo custom runner for macOS.
#
# Codesigns the binary with the Hypervisor.framework entitlement before
# execution. Without this, any code that touches HVF (tests or the daemon)
# fails with HV_ERROR (0xfae94007).
#
# Usage (via .cargo/config.toml):
#   [target.aarch64-apple-darwin]
#   runner = "scripts/codesign-runner.sh"

set -euo pipefail

BINARY="$1"
shift

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
ENTITLEMENTS="$PROJECT_ROOT/entitlements.plist"

if [[ ! -f "$ENTITLEMENTS" ]]; then
    echo "error: entitlements.plist not found at $ENTITLEMENTS" >&2
    exit 1
fi

# Ad-hoc codesign with HVF entitlement (silent unless it fails).
codesign --sign - --entitlements "$ENTITLEMENTS" --force "$BINARY" 2>/dev/null

exec "$BINARY" "$@"
