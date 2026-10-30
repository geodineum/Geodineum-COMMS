#!/bin/bash
# geodineum comms sender — show or change the From a site's email channel sends as.
# Edits {site}:comms:config channels.email.config from_email/from_name, then signals
# settings.reload. COMMS sends as a domain only when it is in authorized-domains.
set -uo pipefail
usage() {
    echo "Usage: sudo geodineum comms sender <site_id> [show | set <email> [--name <display name>]] [--env <dtap>]"
    echo ""
    echo "  show  (default) the current sender"
    echo "  set   send as <email>; its domain must be provisioned first:"
    echo "        sudo bash ~/gh/Geodineum-pro/scripts/setup-mail-stack.sh <domain>"
}
site_id="${1:-}"; [[ "$site_id" == -h || "$site_id" == --help || -z "$site_id" ]] && { usage; [[ -z "$site_id" ]] && exit 2 || exit 0; }
shift; action=show; env_name=production; from=; name=
[[ $# -gt 0 && "$1" != --* ]] && { action="$1"; shift; }
[[ "$action" == set && $# -gt 0 && "$1" != --* ]] && { from="$1"; shift; }
while [[ $# -gt 0 ]]; do case "$1" in --env) env_name="$2"; shift 2 ;; --name) name="$2"; shift 2 ;; *) usage; exit 2 ;; esac; done
case "$action" in
    show) ;;
    set) [[ "$from" =~ ^[^@[:space:]]+@[^@[:space:]]+\.[^@[:space:]]+$ ]] || { echo "set needs an email address" >&2; exit 2; }
         [[ "$name" != *[$'\r\n']* ]] || { echo "--name must be one line" >&2; exit 2; } ;;
    *) usage; exit 2 ;;
esac
cred="${GEODINEUM_CREDENTIALS_DIR:-/etc/geodineum/credentials}/valkey.password"
[[ -r "$cred" ]] || { echo "cannot read $cred — run with sudo" >&2; exit 1; }
vk() { REDISCLI_AUTH="$(<"$cred")" valkey-cli --no-auth-warning --raw -p "${VALKEY_PORT:-47445}" --user default "$@"; }
key="{${site_id}}:comms:config"
current="$(vk GET "$key")"
[[ -n "$current" ]] || { echo "no $key — configure the site's COMMS first" >&2; exit 1; }

read -r -d '' edit <<'PY'
import json, sys
cfg = json.loads(sys.stdin.read())
c = cfg.setdefault("channels", {}).setdefault("email", {"enabled": True, "config": {}}).setdefault("config", {})
if sys.argv[1] == "show":
    print("  %s <%s>" % (c.get("from_name") or "(no name)", c.get("from_email") or "(default sender)"))
    sys.exit(0)
c["from_email"] = sys.argv[2]
if sys.argv[3]:
    c["from_name"] = sys.argv[3]
print(json.dumps(cfg, separators=(",", ":")))
PY
if [[ "$action" == show ]]; then
    echo "$site_id sends as:"; printf '%s' "$current" | python3 -I -c "$edit" show; exit $?
fi
auth=/etc/geodineum/mail/authorized-domains
dom="${from##*@}"
if ! grep -qixF "$dom" "$auth" 2>/dev/null; then
    echo "STOP: $dom is not in $auth — COMMS would fall back to the default sender." >&2
    echo "      Provision it first: sudo bash ~/gh/Geodineum-pro/scripts/setup-mail-stack.sh $dom" >&2
    exit 1
fi
updated="$(printf '%s' "$current" | python3 -I -c "$edit" set "${from,,}" "$name")" || { echo "could not parse $key" >&2; exit 1; }
[[ "$(vk SET "$key" "$updated")" == OK ]] || { echo "SET $key failed" >&2; exit 1; }
id="$(vk XADD "{${site_id}}:gnode:comms:${env_name}" '*' \
    id "$(cat /proc/sys/kernel/random/uuid)" type settings.reload timestamp "$(date -Iseconds)" site_id "$site_id" environment "$env_name" priority 5 \
    sender '{}' content '{}' metadata '{}' dispatch '{"channels":[],"status":"pending"}')"
[[ "$id" =~ ^[0-9]+-[0-9]+$ ]] || echo "warning: settings.reload not published ($id) — restart geodineum-comms to apply" >&2
echo "$site_id sends as:"; printf '%s' "$updated" | python3 -I -c "$edit" show
