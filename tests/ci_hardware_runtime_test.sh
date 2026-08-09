#!/usr/bin/env bash
set -euo pipefail

temp_dir="$(mktemp -d)"
mutated_workflow="$temp_dir/ci.yml"
trap 'rm -f "$mutated_workflow"; rmdir "$temp_dir"' EXIT

sed \
    's|^          cargo test -p visor-vmm net::linux::tests::create_interface_requires_root$|          # cargo test -p visor-vmm net::linux::tests::create_interface_requires_root|' \
    .github/workflows/ci.yml >"$mutated_workflow"

if cmp -s .github/workflows/ci.yml "$mutated_workflow"; then
    echo "hosted TAP mutation fixture no longer matches the workflow" >&2
    exit 1
fi

if CI_HARDWARE_WORKFLOW="$mutated_workflow" bash tests/ci_hardware_runtime.sh >/dev/null 2>&1; then
    echo "hardware runtime contract accepted commented-out hosted TAP coverage" >&2
    exit 1
fi

bash tests/ci_hardware_runtime.sh
echo "Hardware CI runtime regression ok"
