#!/bin/bash
# geodineum comms test-send — push one real message through a site's COMMS stream and
# report the signed receipt's verdict. A written config is a claim; a receipt is proof.
set -uo pipefail
usage() {
    echo "Usage: sudo geodineum comms test-send <site_id> [--env <dtap>] [--channel email|telegram]"
    echo ""
    echo "Sends one test message to the site's CONFIGURED recipients (its {site}:comms:config),"
    echo "through {site}:gnode:comms:<env>, then waits up to 60 s for the receipt COMMS signs."
}
site_id="${1:-}"; [[ "$site_id" == -h || "$site_id" == --help || -z "$site_id" ]] && { usage; [[ -z "$site_id" ]] && exit 2 || exit 0; }
shift; env_name=production; channel=email
while [[ $# -gt 0 ]]; do case "$1" in --env) env_name="$2"; shift 2 ;; --channel) channel="$2"; shift 2 ;; *) usage; exit 2 ;; esac; done
[[ "$channel" == email || "$channel" == telegram ]] || { echo "--channel email|telegram" >&2; exit 2; }
cred="${GEODINEUM_CREDENTIALS_DIR:-/etc/geodineum/credentials}/valkey.password"
[[ -r "$cred" ]] || { echo "cannot read $cred — run with sudo" >&2; exit 1; }
vk() { REDISCLI_AUTH="$(<"$cred")" valkey-cli --no-auth-warning --raw -p "${VALKEY_PORT:-47445}" --user default "$@"; }
stream="{${site_id}}:gnode:comms:${env_name}"
receipts="{${site_id}}:gnode:receipts:${env_name}"
[[ "$(vk EXISTS "{${site_id}}:comms:config")" == 1 ]] || { echo "no {${site_id}}:comms:config — the site has no COMMS settings, nothing to send to" >&2; exit 1; }
ts="$(date -Iseconds)"
id="$(vk XADD "$stream" '*' \
    id "$(cat /proc/sys/kernel/random/uuid)" type contact timestamp "$ts" site_id "$site_id" environment "$env_name" priority normal \
    sender '{"name":"Geodineum COMMS test","email":"","ip":"127.0.0.1","user_agent":"comms-test-send"}' \
    content "{\"subject\":\"Geodineum COMMS test (${site_id})\",\"body\":\"Test message sent at ${ts} by geodineum comms test-send on $(hostname -s). No action needed.\",\"attachments\":[]}" \
    metadata "{\"form_type\":\"comms-test\",\"source_url\":\"cli\",\"environment\":\"${env_name}\"}" \
    dispatch "{\"channels\":[\"${channel}\"],\"status\":\"pending\"}")"
[[ "$id" =~ ^[0-9]+-[0-9]+$ ]] || { echo "XADD to $stream failed: $id" >&2; exit 1; }
echo "published $id on $stream (channel $channel, to the site's configured recipients); waiting for the receipt…"
for i in $(seq 1 30); do
    st="$(vk XREVRANGE "$receipts" + - COUNT 50 | awk -v id="$id" '$0=="cid"{getline c} $0=="st"{getline s; if (c==id) {print s; exit}}')"
    [[ -n "$st" ]] && break; sleep 2
done
if [[ -z "${st:-}" ]]; then echo "NO RECEIPT after 60 s — COMMS did not process $id (geodineum status comms; journalctl -u geodineum-comms -n 50)"; exit 1; fi
echo "receipt: $st  ($receipts, cid $id)"
[[ "$st" == sent ]] && { echo "DELIVERED"; exit 0; } || { echo "NOT DELIVERED ($st) — journalctl -u geodineum-comms -n 50"; exit 1; }
