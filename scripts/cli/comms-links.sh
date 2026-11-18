#!/bin/bash
# geodineum comms links — the other domains a site's mail (button, footer link, logo) may point at.
# Root keeps them in /etc/geodineum/mail/link-domains; COMMS reads the file for every mail, so a change
# needs no reload. A site's own settings cannot add one: the site writes those.
set -uo pipefail
usage() {
    echo "Usage: sudo geodineum comms links [<site_id> [show | set <domain>... | add <domain>... | remove <domain>... | clear]]"
    echo ""
    echo "  (no site)  every site's listed domains, and any site settings still naming link_domains (not read)"
    echo "  show       (default) this site's domains; its own domain (the site id, _ read as .) always counts"
    echo "  set        exactly these domains        add / remove   these domains        clear   none"
    echo "A domain counts with its subdomains."
}
FILE="${COMMS_LINK_DOMAINS_FILE:-/etc/geodineum/mail/link-domains}"
site_id="${1:-}"
[[ "$site_id" == -h || "$site_id" == --help ]] && { usage; exit 0; }
action=list; domains=()
if [[ -n "$site_id" ]]; then
    [[ "$site_id" =~ ^[a-z0-9_-]{1,64}$ ]] || { echo "not a site id: $site_id" >&2; exit 2; }
    shift; action=show
    [[ $# -gt 0 ]] && { action="$1"; shift; }
    domains=("$@")
fi
case "$action" in
    list|show|clear) [[ ${#domains[@]} -eq 0 ]] || { usage; exit 2; } ;;
    set|add|remove) [[ ${#domains[@]} -gt 0 ]] || { echo "$action needs at least one domain" >&2; exit 2; } ;;
    *) usage; exit 2 ;;
esac
for i in "${!domains[@]}"; do
    d="${domains[$i],,}"; d="${d%.}"
    [[ "$d" =~ ^([a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z]([a-z0-9-]{0,61}[a-z0-9])?$ ]] || { echo "not a domain: ${domains[$i]}" >&2; exit 2; }
    domains[$i]="$d"
done
if [[ -e "$FILE" && ! -r "$FILE" ]] || [[ ! -e "$FILE" && -e "$(dirname "$FILE")" && ! -r "$(dirname "$FILE")" ]]; then
    echo "cannot read $FILE — run with sudo" >&2; exit 1
fi

read -r -d '' edit <<'PY'
import sys
path, action, site, doms = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4:]
try:
    lines = open(path).read().splitlines()
except FileNotFoundError:
    lines = []
def words(l):
    return l.split("#", 1)[0].split()
have = [d for l in lines if words(l)[:1] == [site] for d in words(l)[1:]]
have = list(dict.fromkeys(d.lower().rstrip(".") for d in have))
if action == "list":
    for l in lines:
        w = words(l)
        if len(w) > 1:
            print("  %s  %s" % (w[0], " ".join(w[1:])))
    sys.exit(0)
if action == "show":
    print("  " + (" ".join(have) or "(none listed)"))
    sys.exit(0)
new = {"set": doms, "add": have + doms, "remove": [d for d in have if d not in doms], "clear": []}[action]
new = list(dict.fromkeys(new))
out = [l for l in lines if words(l)[:1] != [site]]
if new:
    out.append(site + " " + " ".join(new))
sys.stdout.write("".join(l + "\n" for l in out))
PY

if [[ "$action" == list ]]; then
    echo "domains listed in $FILE:"
    python3 -I -c "$edit" "$FILE" list _ || exit 1
    cred="${GEODINEUM_CREDENTIALS_DIR:-/etc/geodineum/credentials}/valkey.password"
    if [[ -r "$cred" ]]; then
        vk() { REDISCLI_AUTH="$(<"$cred")" valkey-cli --no-auth-warning --raw -p "${VALKEY_PORT:-47445}" --user default "$@"; }
        stale=""
        while IFS= read -r key; do
            [[ -n "$key" ]] || continue
            got="$(vk GET "$key" | python3 -I -c 'import json, sys
try: print(" ".join(json.load(sys.stdin).get("link_domains") or []))
except Exception: pass')"
            [[ -n "$got" ]] && stale+="  ${key}  ${got}"$'\n'
        done < <(vk --scan --pattern '*:comms:config')
        if [[ -n "$stale" ]]; then
            echo "site settings that name link_domains (COMMS does not read these; list a domain above to keep it):"
            printf '%s' "$stale"
        fi
    else
        echo "(site settings not checked: cannot read $cred — run with sudo)"
    fi
    exit 0
fi
if [[ "$action" == show ]]; then
    echo "$site_id's mail may link to its own domain and:"; python3 -I -c "$edit" "$FILE" show "$site_id"; exit $?
fi

dir="$(dirname "$FILE")"
if [[ ! -d "$dir" ]]; then
    install -d -m 0750 -o root -g geodineum-comms "$dir" 2>/dev/null || install -d -m 0750 "$dir" || { echo "cannot create $dir — run with sudo" >&2; exit 1; }
fi
[[ -w "$dir" ]] || { echo "cannot write $dir — run with sudo" >&2; exit 1; }
tmp="$(mktemp "$dir/.link-domains.XXXXXX")" || exit 1
trap 'rm -f "$tmp"' EXIT
python3 -I -c "$edit" "$FILE" "$action" "$site_id" "${domains[@]}" > "$tmp" || { echo "could not edit $FILE" >&2; exit 1; }
chmod 0640 "$tmp"
chgrp geodineum-comms "$tmp" 2>/dev/null || true
mv -f "$tmp" "$FILE" || { echo "could not replace $FILE" >&2; exit 1; }
trap - EXIT
echo "$site_id's mail may link to its own domain and:"; python3 -I -c "$edit" "$FILE" show "$site_id"
