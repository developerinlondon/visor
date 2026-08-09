#!/usr/bin/env bash
set -euo pipefail

temp_dir="$(mktemp -d)"
mutated_workflow="$temp_dir/ci.yml"
mutated_if_workflow="$temp_dir/ci-if.yml"
trap 'rm -f "$mutated_workflow" "$mutated_if_workflow"; rmdir "$temp_dir"' EXIT

sed 's/^    needs: check$/    # needs: check/' .github/workflows/ci.yml >"$mutated_workflow"

if CI_HARDWARE_WORKFLOW="$mutated_workflow" bash tests/ci_hardware_trust.sh >/dev/null 2>&1; then
    echo "hardware trust contract accepted a commented-out hosted-check dependency" >&2
    exit 1
fi

awk '
    /^    if: >-$/ {
        print
        print "      true"
        in_hardware_if = 1
        next
    }
    in_hardware_if && /^    runs-on:/ {
        print "    env:"
        print "      DECOY_TRUST_POLICY: >-"
        for (line_number = 1; line_number <= if_line_count; line_number++) {
            line = if_lines[line_number]
            sub(/^      /, "        ", line)
            print line
        }
        in_hardware_if = 0
        print
        next
    }
    in_hardware_if {
        if_lines[++if_line_count] = $0
        next
    }
    { print }
' .github/workflows/ci.yml >"$mutated_if_workflow"

if CI_HARDWARE_WORKFLOW="$mutated_if_workflow" bash tests/ci_hardware_trust.sh >/dev/null 2>&1; then
    echo "hardware trust contract accepted an allowlist outside the active job condition" >&2
    exit 1
fi

bash tests/ci_hardware_trust.sh
echo "Hardware CI trust regression ok"
