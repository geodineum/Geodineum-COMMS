#!/bin/bash
set -euo pipefail

GEODINEUM_ROOT="${GEODINEUM_ROOT:-/opt/geodineum}"

COMMON="${GEODINEUM_ROOT}/Geodineum/lib/common.sh"
[[ -f "$COMMON" ]] && source "$COMMON"

site_id="${1:-}"
if [[ -z "$site_id" ]]; then
    echo "Usage: geodineum comms test-send <site_id>" >&2
    exit 2
fi

cred_dir="${GEODINEUM_CREDENTIALS_DIR:-/etc/geodineum/credentials}"
port="${VALKEY_PORT:-47445}"

# Use admin credentials for the test publish
admin_pass_file="${cred_dir}/valkey.password"
if [[ ! -f "$admin_pass_file" ]] || [[ ! -r "$admin_pass_file" ]]; then
    echo "Error: cannot read admin credentials at ${admin_pass_file}" >&2
    echo "  Run as root or ensure deploy user is in geodineum-creds group" >&2
    exit 1
fi

admin_pass=$(cat "$admin_pass_file")
stream_key="${site_id}:comms:outbound:email"
timestamp=$(date -Iseconds)

REDISCLI_AUTH="$admin_pass" valkey-cli -p "$port" --user default \
    XADD "$stream_key" '*' \
    type "email" \
    to "test@example.com" \
    subject "Geodineum COMMS test" \
    body "Test message sent at ${timestamp} for site ${site_id}" \
    priority "low" \
    source "cli-test" 2>/dev/null

echo "Test message published to ${stream_key}"
echo "Check delivery: geodineum comms status"
