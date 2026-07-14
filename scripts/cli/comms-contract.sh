#!/bin/bash
set -euo pipefail

# Print the Geodineum-COMMS integration contract (CONTRACT.md) — the
# authoritative message/stream format producers must use. Sourced by
# `geodineum comms contract`.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMMS_ROOT="$(dirname "$(dirname "$SCRIPT_DIR")")"

CONTRACT="${COMMS_ROOT}/CONTRACT.md"

if [[ -r "$CONTRACT" ]]; then
    cat "$CONTRACT"
else
    echo "Error: contract not found at ${CONTRACT}" >&2
    echo "The COMMS component may be incompletely deployed." >&2
    exit 1
fi
