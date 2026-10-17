#!/bin/bash
# geodineum comms recipients — list or change who a site's email channel delivers to.
# Edits {site}:comms:config recipients[] in place, then signals settings.reload so the
# running daemon re-reads it. Addresses live only in ValKey, never in a repo or page.
set -uo pipefail
usage() {
    echo "Usage: sudo geodineum comms recipients <site_id> [list | set <email>... | add <email>... | remove <email>...] [--env <dtap>]"
    echo ""
    echo "  list    (default) the email recipients and their filters"
    echo "  set     replace the recipients with exactly these addresses"
    echo "  add     append addresses (an existing address keeps its filters)"
    echo "  remove  drop addresses"
    echo "New recipients get every message type (types []) at every priority (min_priority 5)."
}
site_id="${1:-}"; [[ "$site_id" == -h || "$site_id" == --help || -z "$site_id" ]] && { usage; [[ -z "$site_id" ]] && exit 2 || exit 0; }
shift; action=list; env_name=production; emails=()
[[ $# -gt 0 && "$1" != --* ]] && { action="$1"; shift; }
while [[ $# -gt 0 ]]; do case "$1" in --env) env_name="$2"; shift 2 ;; -*) usage; exit 2 ;; *) emails+=("$1"); shift ;; esac; done
case "$action" in
    list) ;;
    set|add|remove) [[ ${#emails[@]} -gt 0 ]] || { echo "$action needs at least one address" >&2; exit 2; } ;;
    *) usage; exit 2 ;;
esac
for e in "${emails[@]}"; do [[ "$e" =~ ^[^@[:space:]]+@[^@[:space:]]+\.[^@[:space:]]+$ ]] || { echo "not an email address: $e" >&2; exit 2; }; done
cred="${GEODINEUM_CREDENTIALS_DIR:-/etc/geodineum/credentials}/valkey.password"
[[ -r "$cred" ]] || { echo "cannot read $cred — run with sudo" >&2; exit 1; }
vk() { REDISCLI_AUTH="$(<"$cred")" valkey-cli --no-auth-warning --raw -p "${VALKEY_PORT:-47445}" --user default "$@"; }
key="{${site_id}}:comms:config"
current="$(vk GET "$key")"
[[ -n "$current" ]] || { echo "no $key — configure the site's COMMS first (wp-admin → Comms)" >&2; exit 1; }

read -r -d '' edit <<'PY'
import json, sys
action, emails = sys.argv[1], [e.lower() for e in sys.argv[2:]]
cfg = json.loads(sys.stdin.read())
email = cfg.setdefault("channels", {}).setdefault("email", {"enabled": True, "config": {}})
recips = email.get("recipients") or []
if action == "list":
    for r in recips:
        print("  %s  types=%s  min_priority=%s" % (r.get("email"), r.get("types") or "all", r.get("min_priority", 5)))
    print("  (%d recipient(s); email channel %s)" % (len(recips), "enabled" if email.get("enabled") else "DISABLED"))
    sys.exit(0)
new = lambda e: {"email": e, "types": [], "min_priority": 5}
have = {(r.get("email") or "").lower(): r for r in recips}
if action == "set":
    recips = [have.get(e, new(e)) for e in dict.fromkeys(emails)]
elif action == "add":
    recips += [new(e) for e in dict.fromkeys(emails) if e not in have]
else:
    recips = [r for r in recips if (r.get("email") or "").lower() not in emails]
email["recipients"] = recips
print(json.dumps(cfg, separators=(",", ":")))
PY
if [[ "$action" == list ]]; then
    echo "$site_id email recipients:"; printf '%s' "$current" | python3 -I -c "$edit" list; exit $?
fi
updated="$(printf '%s' "$current" | python3 -I -c "$edit" "$action" "${emails[@]}")" || { echo "could not parse $key" >&2; exit 1; }
[[ "$(vk SET "$key" "$updated")" == OK ]] || { echo "SET $key failed" >&2; exit 1; }
id="$(vk XADD "{${site_id}}:gnode:comms:${env_name}" '*' \
    id "$(cat /proc/sys/kernel/random/uuid)" type settings.reload timestamp "$(date -Iseconds)" site_id "$site_id" environment "$env_name" priority 5 \
    sender '{}' content '{}' metadata '{}' dispatch '{"channels":[],"status":"pending"}')"
[[ "$id" =~ ^[0-9]+-[0-9]+$ ]] || echo "warning: settings.reload not published ($id) — restart geodineum-comms to apply" >&2
echo "$site_id email recipients now:"; printf '%s' "$updated" | python3 -I -c "$edit" list
