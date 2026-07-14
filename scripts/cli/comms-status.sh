#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMMS_ROOT="$(dirname "$(dirname "$SCRIPT_DIR")")"
GEODINEUM_ROOT="${GEODINEUM_ROOT:-/opt/geodineum}"

# Source common.sh for logging
COMMON="${GEODINEUM_ROOT}/Geodineum/lib/common.sh"
[[ -f "$COMMON" ]] && source "$COMMON"

# Service status
if systemctl is-active --quiet geodineum-comms 2>/dev/null; then
    echo -e "  ${GREEN:-}●${NC:-} geodineum-comms: running"
else
    echo -e "  ${RED:-}●${NC:-} geodineum-comms: stopped"
fi

# Stream counts (requires credentials)
cred_dir="${GEODINEUM_CREDENTIALS_DIR:-/etc/geodineum/credentials}"
comms_pass_file="${cred_dir}/valkey_comms.password"
port="${VALKEY_PORT:-47445}"

if [[ -f "$comms_pass_file" ]] && [[ -r "$comms_pass_file" ]]; then
    pass=$(cat "$comms_pass_file" 2>/dev/null)

    echo ""
    echo "  Outbound streams:"
    # Scan for comms streams across sites
    keys=$(REDISCLI_AUTH="$pass" valkey-cli -p "$port" --user gnode_comms \
        KEYS '*:comms:outbound:*' 2>/dev/null || true)
    if [[ -n "$keys" ]]; then
        while IFS= read -r key; do
            [[ -n "$key" ]] || continue
            len=$(REDISCLI_AUTH="$pass" valkey-cli -p "$port" --user gnode_comms \
                XLEN "$key" 2>/dev/null || echo "?")
            echo "    ${key} (${len} messages)"
        done <<< "$keys"
    else
        echo "    (none found)"
    fi
else
    echo ""
    echo "  Cannot read credentials (run as root or gnode user)"
fi

# Recent logs
echo ""
echo "  Recent logs:"
journalctl -u geodineum-comms -n 5 --no-pager 2>/dev/null || echo "    (no journal access)"
