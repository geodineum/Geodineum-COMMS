#!/bin/bash
# geodineum comms links on a scratch file and a throwaway ValKey (:47984): bash tests/comms-links.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")/.." && pwd)"; C="$HERE/scripts/cli/comms-links.sh"
D="$(mktemp -d)"; P=47984
trap 'valkey-cli -p $P -a throwaway --no-auth-warning SHUTDOWN NOSAVE >/dev/null 2>&1; rm -rf "${D:?}"' EXIT
export COMMS_LINK_DOMAINS_FILE="$D/mail/link-domains" GEODINEUM_CREDENTIALS_DIR="$D/creds" VALKEY_PORT=$P
pass=0; fail=0; ok() { pass=$((pass+1)); echo "  ok   $*"; }; bad() { fail=$((fail+1)); echo "  FAIL $*"; }
line() { awk -v s="$1" '$1 == s' "$COMMS_LINK_DOMAINS_FILE"; }

"$C" a_nl set Shop-A.example. b.example >/dev/null && [[ "$(line a_nl)" == "a_nl shop-a.example b.example" ]] && ok "set lowercases and drops the root dot" || bad "set: $(line a_nl)"
[[ "$(stat -c %a "$COMMS_LINK_DOMAINS_FILE")" == 640 ]] && ok "the file is 0640" || bad "mode $(stat -c %a "$COMMS_LINK_DOMAINS_FILE")"
"$C" z_nl add z.example >/dev/null; "$C" a_nl add c.example b.example >/dev/null
[[ "$(line a_nl)" == "a_nl shop-a.example b.example c.example" && "$(line z_nl)" == "z_nl z.example" ]] && ok "add keeps one of each and other sites' lines" || bad "add: $(line a_nl) / $(line z_nl)"
"$C" a_nl remove b.example >/dev/null && [[ "$(line a_nl)" == "a_nl shop-a.example c.example" ]] && ok "remove" || bad "remove: $(line a_nl)"
"$C" z_nl clear >/dev/null && [[ -z "$(line z_nl)" && -n "$(line a_nl)" ]] && ok "clear removes only that site" || bad "clear"
for d in nl 'a.example b' 'https://x.example' '-x.example' 'x..example'; do
    "$C" a_nl set "$d" >/dev/null 2>&1; [[ $? -eq 2 ]] && ok "refused domain '$d'" || bad "accepted domain '$d'"
done
"$C" 'A Site' show >/dev/null 2>&1; [[ $? -eq 2 ]] && ok "refused a site id with a space" || bad "accepted a bad site id"
"$C" a_nl add >/dev/null 2>&1; [[ $? -eq 2 ]] && ok "add with no domain is refused" || bad "add with no domain"
[[ "$(line a_nl)" == "a_nl shop-a.example c.example" ]] && ok "a refused change leaves the file as it was" || bad "file changed: $(line a_nl)"

if command -v valkey-server >/dev/null; then
    mkdir -p "$D/creds" "$D/vk"; printf 'throwaway' > "$D/creds/valkey.password"
    valkey-server --port $P --bind 127.0.0.1 --dir "$D/vk" --save '' --requirepass throwaway --daemonize yes --logfile "$D/vk/v.log" >/dev/null
    for _ in $(seq 50); do valkey-cli -p $P -a throwaway --no-auth-warning PING 2>/dev/null | grep -F PONG >/dev/null && break; sleep 0.1; done
    valkey-cli -p $P -a throwaway --no-auth-warning SET '{s_nl}:comms:config' '{"site_id":"s_nl","channels":{},"link_domains":["pay.evil.example"]}' >/dev/null
    valkey-cli -p $P -a throwaway --no-auth-warning SET '{t_nl}:comms:config' '{"site_id":"t_nl","channels":{}}' >/dev/null
    out="$("$C")"
    [[ "$out" == *"{s_nl}:comms:config  pay.evil.example"* && "$out" != *t_nl* ]] && ok "the listing names site settings that still carry link_domains" || bad "listing: $out"
else echo "  ..   no valkey-server: the listing of site settings is skipped"; fi
echo "== $pass ok, $fail failed"; exit $(( fail > 0 ))
