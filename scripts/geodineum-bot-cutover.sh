#!/bin/bash
# on aesir, as root: moves COMMS from GeodineBot to the Geodineum bot. Asks for tokens at a prompt, never prints one.
#   sudo bash /opt/geodineum/Geodineum-COMMS/scripts/geodineum-bot-cutover.sh preflight   reads only
#   sudo bash /opt/geodineum/Geodineum-COMMS/scripts/geodineum-bot-cutover.sh cutover     GeodineBot goes silent until Geodine polls it on squad
#   sudo bash /opt/geodineum/Geodineum-COMMS/scripts/geodineum-bot-cutover.sh rollback    stop Geodine's Telegram worker on squad first
set -uo pipefail
ENV_FILE="${COMMS_ENV_FILE:-/etc/geodineum/components/geodineum-comms/geodineum-comms.env}"
PW="${CUTOVER_VALKEY_PASSWORD_FILE:-/etc/geodineum/credentials/valkey.password}"
PORT="${VALKEY_PORT:-47445}"
API="${COMMS_TELEGRAM_API:-https://api.telegram.org}"
BIN="${CUTOVER_COMMS_BIN:-$(cd "$(dirname "$0")/.." && pwd)/target/release/geodineum-comms}"
SANDBOX="${CUTOVER_SANDBOX:-}"
OLD=geodine
NEW=geodineum
ENVIRONMENT="${ENVIRONMENT:-production}"

die() { echo "on aesir: STOP: $*" >&2; exit 1; }
say() { printf '\n== %s\n' "$1"; }
[[ -n "$SANDBOX" || ${EUID:-$(id -u)} -eq 0 ]] || die "run with sudo"
[[ -r "$PW" ]] || die "cannot read the ValKey admin credential"
[[ -r "$ENV_FILE" ]] || die "cannot read $ENV_FILE"

vk() { REDISCLI_AUTH="$(<"$PW")" valkey-cli -p "$PORT" --no-auth-warning "$@"; }
# A value on stdin, so that no token is ever on a command line.
vk_set() { vk -x SET "$1" >/dev/null; }
env_get() { local line; line="$(grep -E "^$1=" "$ENV_FILE" | tail -1)" || true; line="${line#*=}"; line="${line%\"}"; line="${line#\"}"; line="${line%\'}"; printf '%s' "${line#\'}"; }
# The token goes to curl in a config on stdin, so it is on no command line.
tg() { local token="$1" method="$2"; shift 2; printf 'url = "%s/bot%s/%s"\n' "$API" "$token" "$method" | curl -sS --max-time 20 -K - "$@"; }
tg_field() { python3 -c 'import json, sys
try:
    d = json.load(sys.stdin)
except ValueError:
    sys.exit()
r = d.get("result")
print(r.get(sys.argv[1], "") if d.get("ok") and isinstance(r, dict) else "")' "$1" 2>/dev/null; }
tg_ok() { python3 -c 'import json, sys
try:
    print("yes" if json.load(sys.stdin).get("ok") else "no")
except ValueError:
    print("no")' 2>/dev/null; }
token_ok() { [[ "$1" =~ ^[0-9]+:[A-Za-z0-9_-]{20,}$ ]]; }
service() { [[ -n "$SANDBOX" ]] || systemctl "$@"; }

SITES_PY='
import hashlib, json, os, sys
current = os.environ.get("CURRENT_TOKEN", "")
others = 0
for line in sys.stdin.read().split("\x00"):
    if not line.strip():
        continue
    key, _, raw = line.partition("\x01")
    site = key.split(":")[0].strip("{}")
    try:
        tg = (json.loads(raw).get("channels") or {}).get("telegram")
    except (ValueError, AttributeError):
        print("  " + site + ": config is not JSON")
        continue
    if not tg:
        continue
    token = str((tg.get("config") or {}).get("bot_token", ""))
    same = bool(token) and token == current
    if same and site != os.environ["OLD_SITE"]:
        others += 1
    state = "on" if tg.get("enabled") else "off"
    digest = hashlib.sha256(token.encode()).hexdigest()[:8] if token else "none"
    print("  " + site + ": telegram " + state + ", token " + digest + ", the bot COMMS polls: " + ("yes" if same else "no"))
sys.exit(3 if others else 0)
'

site_configs() {
    local cursor=0 reply key
    while :; do
        reply="$(vk SCAN "$cursor" MATCH '*:comms:config' COUNT 200)" || return 1
        cursor="$(head -1 <<< "$reply")"
        while read -r key; do
            [[ -n "$key" ]] || continue
            printf '%s\x01' "$key"; vk GET "$key"; printf '\0'
        done < <(tail -n +2 <<< "$reply")
        [[ "$cursor" == 0 ]] && break
    done
}

preflight() {
    local fail=0 current site ids username user out cmd
    say "This build"
    if grep -qa COMMS_ADMIN_BOT "$BIN" 2>/dev/null; then echo "  has the admin bot"; else echo "  NO admin bot in $BIN (wait for the sweep)"; fail=1; fi

    say "COMMS today"
    current="$(env_get TELEGRAM_BOT_TOKEN)"; site="$(env_get COMMS_INBOUND_SITE)"; ids="$(env_get COMMS_ADMIN_IDS)"
    echo "  inbound site: ${site:-unset, so geodine}; admin bot: $(env_get COMMS_ADMIN_BOT | grep . || echo off)"
    if [[ -z "$ids" ]]; then echo "  NO COMMS_ADMIN_IDS: the admin bot would answer nobody"; fail=1; else echo "  operators listed: $(tr ',' '\n' <<< "$ids" | grep -c .)"; fi
    if [[ -n "$current" ]]; then
        username="$(tg "$current" getMe | tg_field username)"
        echo "  polling: @${username:-unknown}"
    else
        echo "  no TELEGRAM_BOT_TOKEN in the env file"
    fi

    say "COMMS's grant on the new site"
    for cmd in "GET {$NEW}:comms:config" "XADD {$NEW}:gnode:comms:inbound:$ENVIRONMENT * a b" "XGROUP CREATE {$NEW}:gnode:comms:inbound:$ENVIRONMENT g 0" \
        "XREADGROUP GROUP g c STREAMS {$NEW}:gnode:comms:$ENVIRONMENT >" "XADD {$NEW}:gnode:comms:workflows:$ENVIRONMENT * a b" "HSET {$NEW}:comms:context:x a b" \
        "SET {$NEW}:comms:active_context:1 x" "SET {$NEW}:gnode:comms:watch x" "XADD {$NEW}:gnode:receipts:$ENVIRONMENT * a b"; do
        # shellcheck disable=SC2086
        out="$(set -f; vk ACL DRYRUN geodineum_comms $cmd 2>&1)"
        if [[ "$out" == OK ]]; then echo "  ok: $cmd"; else echo "  REFUSED: $cmd"; fail=1; fi
    done

    say "Who can read {$NEW}:comms:config, which will hold the admin bot's token"
    while read -r user; do
        [[ -n "$user" ]] || continue
        [[ "$(vk ACL DRYRUN "$user" GET "{$NEW}:comms:config" 2>&1)" == OK ]] && echo "  $user"
    done < <(vk ACL USERS)
    echo "  (only operators and COMMS belong here; anything else: stop and look)"

    say "Telegram channels in site settings"
    out="$(site_configs | CURRENT_TOKEN="$current" OLD_SITE="$OLD" python3 -c "$SITES_PY")"
    case $? in
        0) ;;
        3) fail=1; out+=$'\n'"  ANOTHER SITE holds the token COMMS polls: move or remove that channel first" ;;
        *) fail=1; out+=$'\n'"  could not read the settings" ;;
    esac
    echo "${out:-  none}"
    [[ "$(vk EXISTS "{$NEW}:comms:config")" == 1 ]] && echo "  {$NEW}:comms:config exists: the cutover adds its Telegram channel and keeps the rest"

    say "What the move leaves behind"
    echo "  window accounts linked through COMMS: $(vk --scan --pattern "{$OLD}:gnode:comms:web:*" | grep -c .) (their chat stops continuing the window until the link moves to Geodine)"
    if [[ -z "$SANDBOX" ]]; then
        echo "  pending grant requests (their buttons are on GeodineBot; decide those with: sudo geodineum grants approve|deny <id>):"
        geodineum grants pending 2>/dev/null | sed 's/^/    /'
        for unit in geodineum-comms geodineum-grants-watch; do echo "  $unit: $(systemctl is-active "$unit" 2>/dev/null)"; done
    fi
    return "$fail"
}

# Builds a site's new settings JSON on stdout: the destination's own settings (or a bare record) with the Telegram
# channel of the source, carrying the token from the environment. Nothing else of the source travels.
with_channel() { # <source json> <destination json or empty> <destination site>
    printf '%s\x00%s' "$1" "$2" | TO="$3" python3 -c '
import json, os, sys
src_raw, _, dst_raw = sys.stdin.read().partition("\x00")
src = json.loads(src_raw)
tg = (src.get("channels") or {}).get("telegram")
if not tg or not isinstance(tg.get("config"), dict):
    sys.exit("the source settings have no Telegram channel")
dst = json.loads(dst_raw) if dst_raw.strip() else {"site_id": os.environ["TO"], "enabled": True, "channels": {}}
if not isinstance(dst.get("channels"), dict):
    dst["channels"] = {}
tg = json.loads(json.dumps(tg))
tg["config"]["bot_token"] = os.environ["BOT_TOKEN"]
tg["enabled"] = True
dst["channels"]["telegram"] = tg
dst["site_id"] = os.environ["TO"]
sys.stdout.write(json.dumps(dst))
'
}
without_channel() { # <json>
    printf '%s' "$1" | python3 -c '
import json, sys
c = json.load(sys.stdin)
if isinstance(c.get("channels"), dict):
    c["channels"].pop("telegram", None)
sys.stdout.write(json.dumps(c))
'
}

# The Telegram channel moves from one site to the other. Each value is built and checked before anything is written.
move_channel() { # <from> <to>; the token in BOT_TOKEN
    local from="$1" to="$2" src dst new_to new_from
    src="$(vk GET "{$from}:comms:config")"
    [[ -n "$src" ]] || die "{$from}:comms:config is empty; nothing changed"
    dst="$(vk GET "{$to}:comms:config")"
    new_to="$(with_channel "$src" "$dst" "$to")" || die "could not build {$to}:comms:config; nothing changed"
    new_from="$(without_channel "$src")" || die "could not build {$from}:comms:config; nothing changed"
    [[ "$new_to" == \{* && "$new_from" == \{* ]] || die "the settings did not come out as JSON; nothing changed"
    printf '%s' "$new_to" | vk_set "{$to}:comms:config" || die "could not write {$to}:comms:config; nothing changed"
    printf '%s' "$new_from" | vk_set "{$from}:comms:config" || die "could not strip {$from}:comms:config; the channel is now on BOTH sites: strip it by hand"
}

write_env() { # <site> <admin-bot flag or empty>; the token in BOT_TOKEN
    SITE="$1" ADMIN="$2" ENV_FILE="$ENV_FILE" python3 -c '
import os, re, tempfile
path = os.environ["ENV_FILE"]
with open(path) as f:
    keep = [l for l in f if not re.match(r"(export\s+)?(TELEGRAM_BOT_TOKEN|COMMS_INBOUND_SITE|COMMS_ADMIN_BOT)=", l)]
if keep and not keep[-1].endswith("\n"):
    keep[-1] += "\n"
keep.append("TELEGRAM_BOT_TOKEN=" + os.environ["BOT_TOKEN"] + "\n")
keep.append("COMMS_INBOUND_SITE=" + os.environ["SITE"] + "\n")
if os.environ["ADMIN"]:
    keep.append("COMMS_ADMIN_BOT=" + os.environ["ADMIN"] + "\n")
st = os.stat(path)
fd, tmp = tempfile.mkstemp(dir=os.path.dirname(path), prefix=".cutover.")
os.fchmod(fd, st.st_mode & 0o777)
os.fchown(fd, st.st_uid, st.st_gid)
with os.fdopen(fd, "w") as f:
    f.writelines(keep)
os.replace(tmp, path)
' || die "could not rewrite $ENV_FILE; the site settings have already moved: put the token, COMMS_INBOUND_SITE and COMMS_ADMIN_BOT in by hand and restart geodineum-comms"
}

restart() {
    service restart geodineum-comms || die "geodineum-comms did not restart (journalctl -u geodineum-comms -n 30); the settings and the env file have already moved"
    if service is-active --quiet geodineum-grants-watch; then
        service restart geodineum-grants-watch || echo "  geodineum-grants-watch did not restart: it still watches the old site's stream until it does"
    fi
    return 0
}

cutover() {
    local new_token current new_id old_id username chat answer me
    preflight || die "preflight above has a failure; nothing changed"
    say "Cutover"
    echo "  GeodineBot stops answering from here until Geodine polls it on squad."
    read -rp "  Type yes to go on: " answer; [[ "$answer" == yes ]] || die "not confirmed; nothing changed"
    read -rsp "  The Geodineum bot's token (from BotFather, not shown): " new_token; echo
    token_ok "$new_token" || die "that does not look like a bot token; nothing changed"
    current="$(env_get TELEGRAM_BOT_TOKEN)"
    me="$(tg "$new_token" getMe)"
    new_id="$(tg_field id <<< "$me")"; username="$(tg_field username <<< "$me")"
    [[ -n "$new_id" ]] || die "Telegram does not know that token; nothing changed"
    old_id="$(tg "$current" getMe | tg_field id)"
    [[ "$new_id" != "$old_id" ]] || die "that is the bot COMMS already polls (GeodineBot); nothing changed"
    echo "  It is @$username."
    chat="$(env_get COMMS_WATCH_CHAT)"; chat="${chat:-$(env_get COMMS_ADMIN_IDS | cut -d, -f1)}"
    [[ "$chat" =~ ^-?[0-9]+$ ]] || die "no chat to test with (COMMS_WATCH_CHAT or the first COMMS_ADMIN_IDS); nothing changed"
    echo "  A bot cannot write to you first: open @$username in Telegram and press Start."
    read -rp "  Press Enter when done: " answer
    [[ "$(tg "$new_token" sendMessage --data-urlencode "chat_id=$chat" --data-urlencode "text=Geodineum bot: cutover test, alerts come here from now on." | tg_ok)" == yes ]] \
        || die "the bot could not write to chat $chat (Start not pressed?); nothing changed"
    export BOT_TOKEN="$new_token"; new_token=''
    move_channel "$OLD" "$NEW"
    write_env "$NEW" true
    unset BOT_TOKEN
    restart
    say "Done"
    echo "  COMMS polls @$username for site $NEW. GeodineBot's token is off the env file and the site settings."
    [[ -n "$SANDBOX" ]] || geodineum comms test-send "$NEW" --channel telegram
    echo "  next, on squad: start Geodine's Telegram worker with GeodineBot's token. It sat on this server, so regenerate it at BotFather first and use the new one."
    echo "  back out before that worker starts: this script with rollback"
}

rollback() {
    local old_token answer src dst new_old new_new
    say "Rollback"
    echo "  COMMS goes back to polling GeodineBot. Stop Geodine's Telegram worker on squad first: one token, one poller."
    read -rp "  Type yes to go on: " answer; [[ "$answer" == yes ]] || die "not confirmed; nothing changed"
    read -rsp "  GeodineBot's token (not shown): " old_token; echo
    token_ok "$old_token" || die "that does not look like a bot token; nothing changed"
    [[ -n "$(tg "$old_token" getMe | tg_field id)" ]] || die "Telegram does not know that token; nothing changed"
    src="$(vk GET "{$NEW}:comms:config")"
    [[ -n "$src" ]] || die "{$NEW}:comms:config is empty, so its Telegram channel cannot move back; nothing changed"
    dst="$(vk GET "{$OLD}:comms:config")"
    export BOT_TOKEN="$old_token"; old_token=''
    new_old="$(with_channel "$src" "$dst" "$OLD")" || die "could not build {$OLD}:comms:config; nothing changed"
    new_new="$(without_channel "$src")" || die "could not build {$NEW}:comms:config; nothing changed"
    [[ "$new_old" == \{* && "$new_new" == \{* ]] || die "the settings did not come out as JSON; nothing changed"
    printf '%s' "$new_old" | vk_set "{$OLD}:comms:config" || die "could not write {$OLD}:comms:config; nothing changed"
    printf '%s' "$new_new" | vk_set "{$NEW}:comms:config" || die "could not strip {$NEW}:comms:config; the channel is now on BOTH sites: strip it by hand"
    write_env "$OLD" ""
    unset BOT_TOKEN
    restart
    echo "  COMMS polls GeodineBot again, as before the cutover. The Geodineum bot's token is off the env file and the site settings."
}

case "${1:-}" in
    preflight) preflight; rc=$?; say "Result"; if [[ $rc -eq 0 ]]; then echo "  ready"; else echo "  NOT ready, see above"; fi; exit $rc ;;
    cutover) cutover ;;
    rollback) rollback ;;
    *) sed -n 2,5p "$0"; exit 2 ;;
esac
