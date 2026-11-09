#!/bin/bash
# The Geodineum bot, proven with the real COMMS binary under COMMS's real ACL grant (as install.sh ships it), against a
# throwaway ValKey and a fake Bot API, both on localhost ports of their own. Touches nothing live; needs no root.
#   bash scripts/admin-bot-proof.sh
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ACL_SRC="${ACL_SRC:-$HOME/gh/Geodineum-pro/install.sh}"
COMMS="$ROOT/target/release/geodineum-comms"
P=${PROOF_VALKEY_PORT:-47996}; TP=${PROOF_TELEGRAM_PORT:-47995}

die() { echo "STOP: $*" >&2; exit 1; }
for port in "$P" "$TP"; do
    [[ "$port" =~ ^[0-9]+$ && "$port" -ne 47445 && "$port" -gt 1024 ]] || die "port $port: not a throwaway port"
    (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null && die "port $port is in use; set PROOF_VALKEY_PORT / PROOF_TELEGRAM_PORT"
done
[[ "$P" != "$TP" ]] || die "the two ports must differ"
[[ -x "$COMMS" ]] || die "no release build at $COMMS (cargo build --release)"
[[ -r "$ACL_SRC" ]] || die "cannot read the grant source $ACL_SRC"
command -v valkey-server >/dev/null || die "valkey-server not found"

W="$(mktemp -d)"; chmod 700 "$W"
S="$W/spool"; mkdir -p "$S" "$W/data"
pass=0; failed=0
CHILDREN=()
COMMS_PID=''
cleanup() {
    [[ -n "$COMMS_PID" ]] && stop_comms
    local p
    for p in "${CHILDREN[@]}"; do kill "$p" 2>/dev/null; wait "$p" 2>/dev/null; done
    rm -rf "$W"
}
trap cleanup EXIT
check() { if [[ "$2" == "$3" ]]; then pass=$((pass+1)); echo "ok   $1"; else failed=$((failed+1)); echo "FAIL $1: wanted [$3], got [$2]"; fi; }
until_() { local n="$1"; shift; for _ in $(seq "$n"); do "$@" >/dev/null 2>&1 && return 0; sleep 0.1; done; return 1; }
vk() { REDISCLI_AUTH="$(<"$W/admin.pw")" valkey-cli -p "$P" --no-auth-warning "$@"; }
has_group() { [[ "$(vk XINFO GROUPS "$1" 2>/dev/null)" == *geodineum_comms_inbound* ]]; }
fake_up() { [[ "$(curl -s "http://127.0.0.1:$TP/bot0/getMe")" == *'"ok": true'* ]]; }

# ---- the throwaway ValKey, with COMMS's real grant --------------------------------------------------------------------
umask 077
openssl rand -hex 16 | tr -d '\n' > "$W/admin.pw"
openssl rand -hex 16 | tr -d '\n' > "$W/comms.pw"
printf 'port %s\nbind 127.0.0.1\nrequirepass %s\nsave ""\nappendonly no\ndir %s\nlogfile %s\ndaemonize no\n' "$P" "$(<"$W/admin.pw")" "$W" "$W/valkey.log" > "$W/valkey.conf"
valkey-server "$W/valkey.conf" >/dev/null 2>&1 &
CHILDREN+=($!)
until_ 50 vk PING || die "the throwaway ValKey did not start: $(tail -3 "$W/valkey.log")"

# The grant exactly as the installer gives it: its two arrays, read by the shell.
grant_array() { awk -v name="local $1=(" 'index($0, name) { f=1; next } f && /^    \)$/ { exit } f' "$ACL_SRC"; }
eval "CMDS=($(grant_array acl_comms_commands))"; eval "KEYS=($(grant_array acl_comms_keys))"
(( ${#CMDS[@]} > 20 && ${#KEYS[@]} > 10 )) || die "could not read the grant from $ACL_SRC (${#CMDS[@]} commands, ${#KEYS[@]} keys)"
# The ACL line goes to valkey-cli on stdin: the password is on no command line.
applied="$( { printf 'ACL SETUSER geodineum_comms on >%s resetkeys clearselectors resetchannels &*' "$(<"$W/comms.pw")"; printf ' "%s"' "${CMDS[@]}" "${KEYS[@]}"; printf '\n'; } | vk)"
[[ "$applied" == OK ]] || die "the grant was not applied: $applied"
check "the grant is the real one: COMMS reads no site's own data" "$(vk ACL DRYRUN geodineum_comms GET '{nierto_com}:gcore:cache:x' | grep -c 'no permissions')" 1
check "the grant is the real one: KEYS refused, HSETNX on a context allowed" "$(vk ACL DRYRUN geodineum_comms KEYS '*' | grep -c 'no permissions'):$(vk ACL DRYRUN geodineum_comms HSETNX '{x}:comms:context:p' f v)" 1:OK

# ---- the fake Bot API ---------------------------------------------------------------------------------------------------
python3 -I "$ROOT/tests/fake_telegram.py" "$TP" "$S" &
CHILDREN+=($!)
until_ 50 fake_up || die "the fake Bot API did not start"
: > "$S/calls.jsonl"

# Two made-up tokens, in files only. 111111 is GeodineBot's stand-in, 222222 the Geodineum bot's.
printf '111111:%s' "$(openssl rand -hex 20 | tr -d '\n')" > "$W/tok.legacy"
printf '222222:%s' "$(openssl rand -hex 20 | tr -d '\n')" > "$W/tok.admin"
sha() { sha256sum "$1" | cut -c1-12; }   # the files hold the token with no newline, as the fake server hashes it
LEGACY_SHA="$(sha "$W/tok.legacy")"; ADMIN_SHA="$(sha "$W/tok.admin")"
{ cat "$W/tok.legacy"; echo; cat "$W/tok.admin"; echo; } > "$W/tokens.txt"   # grep's pattern file for the leak checks
leaks() { grep -c -F -f "$W/tokens.txt" "$1"; }

# ---- COMMS ---------------------------------------------------------------------------------------------------------------
printf 'consumer:\n  block_ms: 200\n  batch_size: 20\n  idle_claim_ms: 30000\n  discovery_interval_secs: 1\n' > "$W/comms.yaml"
# An env file like the unit's EnvironmentFile: the token enters the process from the file, never from an argument.
mkenv() { # <env file> <token file> [KEY=VALUE ...]
    local out="$1" tf="$2"; shift 2
    {
        printf 'VALKEY_HOST=127.0.0.1\nVALKEY_PORT=%s\nVALKEY_USER=geodineum_comms\nVALKEY_AUTH_FILE=%s\n' "$P" "$W/comms.pw"
        printf 'GEODINEUM_COMMS_DATA_DIR=%s\nNODE_ID=proof\nENVIRONMENT=production\nGNODE_TOPOLOGY_NAMESPACE=constellation\nLOG_LEVEL=info\n' "$W/data"
        printf 'COMMS_TELEGRAM_API=http://127.0.0.1:%s\n' "$TP"
        printf 'TELEGRAM_BOT_TOKEN=%s\n' "$(<"$tf")"
        printf '%s\n' "$@"
    } > "$out"
}
comms() { # <env file>; only an env file that points at the fake API and this ValKey is ever started
    [[ "$(env_value "$1" COMMS_TELEGRAM_API)" == "http://127.0.0.1:$TP" && "$(env_value "$1" VALKEY_PORT)" == "$P" ]] || die "refusing to start COMMS from $1: not aimed at the throwaways"
    ( cd "$ROOT" && exec env -i PATH="$PATH" HOME="$W" bash -c 'set -a; . "$1"; set +a; exec "$2" --config "$3"' _ "$1" "$COMMS" "$W/comms.yaml" ) >> "$W/comms.log" 2>&1 </dev/null &
    COMMS_PID=$!
}
# After a stop, the fake API may still hold that bot's last poll open for a second; a new poller inside that second would
# get the real API's 409 and back off, so wait it out.
stop_comms() { kill "$COMMS_PID" 2>/dev/null; wait "$COMMS_PID" 2>/dev/null; COMMS_PID=''; sleep 1.5; }

# ---- helpers: what a person does, what the bot did ----------------------------------------------------------------------
seq_=0
msg() { # <bot id> <from> <text>
    seq_=$((seq_+1)); mkdir -p "$S/$1"
    python3 -I - "$S/$1/$(printf %05d "$seq_").json" "$2" "$3" <<'PY'
import json, sys
p, who, text = sys.argv[1:4]
json.dump({"message": {"message_id": 1, "from": {"id": int(who), "first_name": "U" + who, "is_bot": False},
                       "chat": {"id": int(who), "type": "private"}, "date": 1, "text": text}}, open(p, "w"))
PY
}
press() { # <bot id> <from> <callback data>; sets LAST_CB
    seq_=$((seq_+1)); mkdir -p "$S/$1"; LAST_CB="cb$seq_"
    python3 -I - "$S/$1/$(printf %05d "$seq_").json" "$2" "$3" "$LAST_CB" <<'PY'
import json, sys
p, who, data, cid = sys.argv[1:5]
json.dump({"callback_query": {"id": cid, "from": {"id": int(who), "first_name": "U" + who, "is_bot": False}, "data": data,
                              "message": {"message_id": 7, "chat": {"id": int(who), "type": "private"}, "date": 1}}}, open(p, "w"))
PY
}
calls() { # <method> [field=value ...]: how many calls match (fields of the call, then of its body)
    python3 -I - "$S/calls.jsonl" "$@" <<'PY'
import json, sys
p, method, *f = sys.argv[1:]
n = 0
for l in open(p):
    c = json.loads(l)
    if c["method"] != method:
        continue
    if all(str(c.get(k, c["body"].get(k))) == v for k, v in (x.split("=", 1) for x in f)):
        n += 1
print(n)
PY
}
said() { # <chat> <text fragment>: how many messages to that chat carry it
    python3 -I - "$S/calls.jsonl" "$1" "$2" <<'PY'
import json, sys
p, chat, needle = sys.argv[1:4]
print(sum(1 for l in open(p) for c in [json.loads(l)]
          if c["method"] == "sendMessage" and str(c["body"].get("chat_id")) == chat and needle in str(c["body"].get("text"))))
PY
}
keyboards() { python3 -I - "$S/calls.jsonl" <<'PY'
import json, sys
print(sum("reply_markup" in c["body"] for c in map(json.loads, open(sys.argv[1])) if c["method"] == "sendMessage"))
PY
}
menu_size() { python3 -I - "$S/calls.jsonl" <<'PY'
import json, sys
sizes = [len(c["body"]["commands"]) for c in map(json.loads, open(sys.argv[1])) if c["method"] == "setMyCommands"]
print(sizes[-1] if sizes else "none")
PY
}
foreign_calls() { python3 -I - "$S/calls.jsonl" "$1" <<'PY'
import json, sys
print(sum(c["token_sha"] != sys.argv[2] for c in map(json.loads, open(sys.argv[1]))))
PY
}
polls_only() { python3 -I - "$S/calls.jsonl" "$1" <<'PY'
import json, sys
cs = [c for c in map(json.loads, open(sys.argv[1])) if c["method"] == "getUpdates"]
print("yes" if cs and all(c["token_sha"] == sys.argv[2] for c in cs) else "no")
PY
}
settings_token_sha() { # <site>: sha of the token in that site's Telegram channel, or none
    vk GET "{$1}:comms:config" | python3 -I -c '
import hashlib, json, sys
try:
    t = json.load(sys.stdin)["channels"]["telegram"]["config"]["bot_token"]
    print(hashlib.sha256(t.encode()).hexdigest()[:12])
except (ValueError, KeyError, TypeError):
    print("none")'
}
settings_field() { vk GET "{$1}:comms:config" | python3 -I -c 'import json, sys; print(json.load(sys.stdin).get(sys.argv[1], ""))' "$2"; }
env_value() { sed -n "s/^$2=//p" "$1" | tail -1; }
env_token_sha() { local v; v="$(env_value "$1" TELEGRAM_BOT_TOKEN)"; [[ -n "$v" ]] && printf '%s' "$v" | sha256sum | cut -c1-12 || echo none; }
set_settings() { # <site> <token file or -> : the site's COMMS settings with a Telegram channel (or none)
    local tf="$2"; [[ "$tf" == - ]] && tf=/dev/null
    SITE="$1" TOKEN="$(<"$tf")" python3 -I -c '
import json, os
c = {"site_id": os.environ["SITE"], "enabled": True, "channels": {}}
if os.environ["TOKEN"]:
    c["channels"]["telegram"] = {"enabled": True, "config": {"bot_token": os.environ["TOKEN"], "chat_id": "1"},
                                 "recipients": [{"chat_id": "1", "types": [], "min_priority": 5}]}
print(json.dumps(c), end="")' | vk -x SET "{$1}:comms:config" >/dev/null
}
outbound() { # <site> <metadata json> [to json]
    local args=(); [[ -n "${3:-}" ]] && args=(to "$3")
    vk XADD "{$1}:gnode:comms:production" '*' id "$(cat /proc/sys/kernel/random/uuid)" type alert timestamp "$(date -Iseconds)" site_id "$1" environment production priority 2 \
        sender '{"name":"proof","email":"","ip":"127.0.0.1","user_agent":"proof"}' content '{"subject":"Proof alert","body":"the body","attachments":[]}' \
        metadata "$2" dispatch '{"channels":["telegram"],"status":"pending"}' "${args[@]}" >/dev/null
}
settle() { sleep "${1:-2}"; }

# =========================================================================================================================
echo "--- the old configuration (site geodine, the old token, no switch): no conversation is left to reach"
set_settings geodine "$W/tok.legacy"
mkenv "$W/env.old" "$W/tok.legacy" COMMS_ADMIN_IDS=1 COMMS_INBOUND_SITE=geodine
comms "$W/env.old"
until_ 100 has_group '{geodine}:gnode:comms:inbound:production' || die "COMMS never joined its inbound stream: $(tail -5 "$W/comms.log")"
settle 1
msg 111111 1 "hello model"; msg 111111 1 "/pipeline aurelius"; settle
check "free text and /pipeline: told where to go" "$(said 1 'no conversations')" 2
check "nothing reaches Geodine's stream" "$(vk EXISTS '{geodine}:gnode:unified:production')" 0
msg 111111 2 "/link geodineum_com-ABCDEFGH23"; settle
check "a stranger cannot try a window link any more" "$(calls sendMessage chat_id=2)" 0
stop_comms

echo "--- the Geodineum bot, site geodineum"
set_settings geodineum "$W/tok.admin"
vk DEL '{geodine}:comms:config' >/dev/null
: > "$S/calls.jsonl"; U0="$(vk XLEN '{geodine}:gnode:unified:production')"
vk SET '{geodineum}:gnode:comms:web:2' '{"site":"geodineum_com","consumer":"web:0123456789abcdef","chat":"2","at_ms":1}' >/dev/null
mkenv "$W/env.on" "$W/tok.admin" COMMS_ADMIN_IDS=1 COMMS_INBOUND_SITE=geodineum
comms "$W/env.on"
until_ 100 has_group '{geodineum}:gnode:comms:inbound:production' || die "COMMS never joined its inbound stream: $(tail -5 "$W/comms.log")"
settle 1
check "the bot menu is /status and /help" "$(menu_size)" 2
msg 222222 1 "hello model"; settle
check "free text: told where to go" "$(said 1 'no conversations')" 1
check "free text: nothing on Geodine's stream" "$(vk XLEN '{geodine}:gnode:unified:production')" "$U0"
check "free text: nothing on the bot site's stream" "$(vk EXISTS '{geodineum}:gnode:unified:production')" 0
check "no conversation record" "$(vk EXISTS '{geodineum}:comms:conversation:1')" 0
for c in "/pipeline aurelius" /history /reset /new; do msg 222222 1 "$c"; done; settle
check "/pipeline /history /reset /new: told where to go" "$(said 1 'no conversations')" 5
check "...and nothing switched" "$(said 1 'Pipeline switched')" 0
msg 222222 1 "/status"; msg 222222 1 "/help"; msg 222222 1 "/start"; settle
check "/status answers" "$(said 1 'Geodineum-COMMS is running')" 1
check "/help and /start answer, without a pipeline" "$(said 1 'grant requests')" 2
check "no menu keyboard from /start" "$(keyboards)" 0
I0="$(vk XLEN '{geodineum}:gnode:comms:inbound:production')"
for t in "hello" "/link geodineum_com-ABCDEFGH23" "/start geodineum_com-ABCDEFGH23" "/unlink" "/status"; do msg 222222 2 "$t"; done; settle
check "a stranger gets no answer, even holding an old link record" "$(calls sendMessage chat_id=2)" 0
check "a stranger reaches no stream" "$(vk XLEN '{geodineum}:gnode:comms:inbound:production')" "$I0"
press 222222 1 "grant:approve:gr-1-1"; CB=$LAST_CB; settle
check "a grant press is on the stream the grants watcher reads" "$(vk XRANGE '{geodineum}:gnode:comms:inbound:production' - + | grep -c 'grant:approve:gr-1-1')" 1
check "a grant press is answered once" "$(calls answerCallbackQuery callback_query_id=$CB)" 1
vk XGROUP CREATE '{geodineum}:gnode:comms:inbound:production' grants-watch 0 >/dev/null
check "the grants watcher's own group sees it" "$(vk XREADGROUP GROUP grants-watch w COUNT 100 STREAMS '{geodineum}:gnode:comms:inbound:production' '>' | grep -c 'grant:approve:gr-1-1')" 1
press 222222 2 "grant:approve:gr-2-2"; settle
check "a stranger's grant press reaches nothing" "$(vk XRANGE '{geodineum}:gnode:comms:inbound:production' - + | grep -c 'gr-2-2')" 0
msg 222222 1 "UPDATE ALL"; settle
check "a workflow intent with no engine listening is refused, and the operator told" "$(vk EXISTS '{geodineum}:gnode:comms:workflows:production'):$(said 1 'no workflow engine')" 0:1
vk XGROUP CREATE '{geodineum}:gnode:comms:workflows:production' engine 0 MKSTREAM >/dev/null
msg 222222 1 "UPDATE ALL"; settle
check "a workflow intent is dispatched on the bot site once an engine listens" "$(vk XLEN '{geodineum}:gnode:comms:workflows:production')" 1
msg 222222 1 "HELLO THERE"; settle
check "an unknown upper-case line: told where to go, no inference" "$(said 1 'no conversations'):$(vk EXISTS '{geodineum}:gnode:unified:production')" 6:0

: > "$S/calls.jsonl"
outbound geodineum '{"form_type":"proof"}'; settle 4
check "an alert on the bot site goes out on the admin bot, to its chat" "$(calls sendMessage bot=222222 chat_id=1)" 1
outbound geodineum '{"form_type":"proof","reply_options":["DISMISS","RETRY"],"context_id":"ctx-1","component":"proof","callback_stream":"{geodineum}:gnode:comms:inbound:replies","chat_id":"1"}'; settle 4
msg 222222 1 "DISMISS"; settle
check "a reply to an alert reaches its component" "$(vk XRANGE '{geodineum}:gnode:comms:inbound:replies' - + | grep -c 'ctx-1')" 1
outbound geodineum '{"form_type":"proof","reply_options":["Yes","No"],"context_id":"poll-1","component":"proof","callback_stream":"{geodineum}:gnode:comms:inbound:votes"}'; settle 4
press 222222 1 "poll:geodineum:poll-1:1"; settle
check "a poll vote reaches its component" "$(vk XRANGE '{geodineum}:gnode:comms:inbound:votes' - + | grep -c 'poll_vote')" 1
vk SET '{alpha_site}:comms:config' '{"site_id":"alpha_site","enabled":true,"channels":{},"addressed":{"types":["alert","alpha"],"daily_cap":200}}' >/dev/null
vk SET '{geodineum}:gnode:comms:web:9' '{"site":"alpha_site","consumer":"web:0123456789abcdef","chat":"9","at_ms":1}' >/dev/null
: > "$S/calls.jsonl"
outbound alpha_site '{"form_type":"proof"}' '{"telegram":"web:0123456789abcdef"}'; settle 4
check "a Telegram message addressed to a person is refused: nobody's chat gets it" "$(calls sendMessage)" 0
check "only the admin bot's token was ever used" "$(foreign_calls "$ADMIN_SHA")" 0
check "COMMS hit no ACL refusal" "$(grep -c NOPERM "$W/comms.log")" 0
stop_comms

echo "--- no operators listed"
: > "$W/comms.log"; : > "$S/calls.jsonl"
mkenv "$W/env.nobody" "$W/tok.admin" COMMS_INBOUND_SITE=geodineum
comms "$W/env.nobody"
settle 3
msg 222222 1 "/status"; settle
check "fail-closed: nobody is answered" "$(calls sendMessage)" 0
check "fail-closed: the log says why" "$(grep -c 'answers nobody' "$W/comms.log")" 1
stop_comms

check "no token ever reached the fake API's log" "$(leaks "$S/calls.jsonl")" 0

echo; echo "passed $pass, failed $failed"
[[ $failed -eq 0 ]]
