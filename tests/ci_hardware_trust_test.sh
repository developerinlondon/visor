#!/usr/bin/env bash
set -euo pipefail

temp_dir="$(mktemp -d)"
mutated_workflow="$temp_dir/ci.yml"
trap 'rm -f "$mutated_workflow"; rmdir "$temp_dir"' EXIT

sed 's/^    needs: check$/    # needs: check/' .github/workflows/ci.yml >"$mutated_workflow"

if CI_HARDWARE_WORKFLOW="$mutated_workflow" bash tests/ci_hardware_trust.sh >/dev/null 2>&1; then
    echo "hardware trust contract accepted a commented-out hosted-check dependency" >&2
    exit 1
fi

bash tests/ci_hardware_trust.sh
echo "Hardware CI trust regression ok"
