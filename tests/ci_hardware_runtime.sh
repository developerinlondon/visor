#!/usr/bin/env bash
set -euo pipefail

workflow="${CI_HARDWARE_WORKFLOW:-.github/workflows/ci.yml}"
check_job="$({
    awk '
        /^  check:$/ { in_check = 1 }
        in_check && /^  [A-Za-z0-9_-]+:$/ && $0 != "  check:" { exit }
        in_check && $0 !~ /^[[:space:]]*#/ { print }
    ' "$workflow"
})"
hardware_job="$({
    awk '
        /^  hardware:$/ { in_hardware = 1 }
        in_hardware && /^  [A-Za-z0-9_-]+:$/ && $0 != "  hardware:" { exit }
        in_hardware { print }
    ' "$workflow"
})"

require_hardware_text() {
    local expected=$1
    if ! grep -Fq -- "$expected" <<<"$hardware_job"; then
        echo "hardware job is missing runtime contract: $expected" >&2
        exit 1
    fi
}

if ! grep -Eq '^[[:space:]]*bash tests/ci_hardware_runtime_test[.]sh[[:space:]]*$' <<<"$check_job"; then
    echo "hosted CI must enforce the hardware runtime contract" >&2
    exit 1
fi

if ! grep -Eq '^[[:space:]]*cargo test -p visor-vmm net::linux::tests::create_interface_requires_root[[:space:]]*$' <<<"$check_job"; then
    echo "hosted CI must exercise the non-root TAP contract" >&2
    exit 1
fi

require_hardware_text 'os.open(path, os.O_RDWR | os.O_CLOEXEC)'
require_hardware_text "short_root=\"/tmp/visor-\${GITHUB_RUN_ID}-\${GITHUB_RUN_ATTEMPT}\""
require_hardware_text "VISOR_TEST_TEMP_ROOT=\$short_root/tests"
require_hardware_text "VISOR_VSOCK_SOCKET_DIR=\$short_root/vsock"
require_hardware_text '--skip net::linux::tests::create_interface_requires_root'

if grep -Fq 'test -r /dev/kvm && test -w /dev/kvm' <<<"$hardware_job"; then
    echo "hardware access must be verified by opening the device, not mode bits" >&2
    exit 1
fi

echo "Hardware CI runtime contract ok"
