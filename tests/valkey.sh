#!/bin/bash
# The ValKey-backed tests against a throwaway server (:47997).
set -euo pipefail
cd "$(dirname "$0")/.."
P=${COMMS_TEST_VALKEY_PORT:-47997}
W="$(mktemp -d)"; trap 'valkey-cli -p $P shutdown nosave >/dev/null 2>&1 || true; rm -rf "$W"' EXIT
valkey-server --port "$P" --save '' --appendonly no --daemonize yes --dir "$W" --logfile "$W/v.log"
for _ in $(seq 50); do valkey-cli -p "$P" ping >/dev/null 2>&1 && break; sleep 0.1; done
COMMS_TEST_VALKEY_PORT=$P cargo test --offline --test addressed --test web_link --test watch "$@"
