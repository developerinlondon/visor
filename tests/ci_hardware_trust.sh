#!/usr/bin/env bash
set -euo pipefail

workflow="${CI_HARDWARE_WORKFLOW:-.github/workflows/ci.yml}"
hardware_job="$(
    awk '
        /^  hardware:$/ { in_hardware = 1 }
        in_hardware && /^  [A-Za-z0-9_-]+:$/ && $0 != "  hardware:" { exit }
        in_hardware { print }
    ' "$workflow"
)"
hardware_job_code="$(sed '/^[[:space:]]*#/d' <<<"$hardware_job")"
hardware_if="$(
    awk '
        /^    if: >-$/ { in_if = 1; next }
        in_if && /^    [A-Za-z0-9_-]+:/ { exit }
        in_if { print }
    ' <<<"$hardware_job_code"
)"
workflow_permissions="$(
    awk '
        /^permissions:$/ { in_permissions = 1; next }
        in_permissions && /^[^[:space:]]/ { exit }
        in_permissions && NF { print }
    ' "$workflow"
)"

if [[ -z "$hardware_job" ]]; then
    echo "ci workflow must define the hardware job" >&2
    exit 1
fi

require_hardware_text() {
    local expected=$1
    if ! grep -Fq "$expected" <<<"$hardware_job_code"; then
        echo "hardware job is missing trust control: $expected" >&2
        exit 1
    fi
}

if [[ "$workflow_permissions" != "  contents: read" ]]; then
    echo "ci workflow permissions must be exactly contents: read" >&2
    exit 1
fi

require_hardware_text "runs-on: [self-hosted, linux, x64, visor-kvm]"

if ! grep -Eq '^    needs: check([[:space:]]*#.*)?$' <<<"$hardware_job"; then
    echo "hardware job must depend on the hosted check job" >&2
    exit 1
fi

if ! grep -Eq '^    if: >-$' <<<"$hardware_job_code"; then
    echo "hardware job event allowlist must be an active job condition" >&2
    exit 1
fi

hardware_if_normalized="$(tr -d '[:space:]' <<<"$hardware_if")"
expected_hardware_if="(github.event_name=='push'&&github.ref=='refs/heads/main')||github.event_name=='workflow_dispatch'||(github.event_name=='pull_request'&&github.event.pull_request.head.repo.full_name==github.repository)"
if [[ "$hardware_if_normalized" != "$expected_hardware_if" ]]; then
    echo "hardware job condition must be the exact trusted-event allowlist" >&2
    exit 1
fi

if grep -Fq "pull_request_target:" "$workflow"; then
    echo "hardware CI must not use pull_request_target" >&2
    exit 1
fi

if grep -Eq '^    permissions:' <<<"$hardware_job"; then
    echo "hardware job must inherit the read-only workflow permissions" >&2
    exit 1
fi

mapfile -t actions < <(sed -n 's/^[[:space:]]*- uses: \([^[:space:]]*\)$/\1/p' <<<"$hardware_job")
if ((${#actions[@]} == 0)); then
    echo "hardware job must declare its actions" >&2
    exit 1
fi

for action in "${actions[@]}"; do
    if [[ ! "$action" =~ ^[^@[:space:]]+@[0-9a-f]{40}$ ]]; then
        echo "hardware action must use an immutable commit: $action" >&2
        exit 1
    fi
done

echo "Hardware CI trust contract ok"
