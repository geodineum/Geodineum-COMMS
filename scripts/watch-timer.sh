#!/bin/bash
# on aesir, as root: install the hourly COMMS watch (config/geodineum-comms-watch.{service,timer}) and run it once.
#   sudo bash /opt/geodineum/Geodineum-COMMS/scripts/watch-timer.sh [--remove]
# It alerts on Telegram (the bot COMMS holds; chat COMMS_WATCH_CHAT or the first COMMS_ADMIN_IDS) and mails the
# operator once a day through COMMS. Idempotent.
set -uo pipefail
HERE="$(cd "$(dirname "$0")/.." && pwd)"
D=/etc/systemd/system
die() { echo "on aesir: STOP: $*" >&2; exit 1; }
[[ $EUID -eq 0 ]] || die "run with sudo"
if [[ "${1:-}" == "--remove" ]]; then
    systemctl disable --now geodineum-comms-watch.timer 2>/dev/null; rm -f "$D"/geodineum-comms-watch.{service,timer}; systemctl daemon-reload
    echo "on aesir: geodineum-comms-watch removed"; exit 0
fi
[[ -x "$HERE/target/release/geodineum-comms" ]] || die "no COMMS build at $HERE/target/release (wait for the sweep)"
"$HERE/target/release/geodineum-comms" --help 2>/dev/null | grep -q ' watch ' || die "this COMMS build has no watch command yet (wait for the sweep)"
install -m 0640 -o root -g root "$HERE/config/geodineum-comms-watch.service" "$HERE/config/geodineum-comms-watch.timer" "$D/"
systemctl daemon-reload
systemctl enable --now geodineum-comms-watch.timer >/dev/null 2>&1 || die "the timer did not start"
systemctl start geodineum-comms-watch.service || die "the first watch failed: journalctl -u geodineum-comms-watch -n 20"
journalctl -u geodineum-comms-watch -n 5 --no-pager | sed 's/^/  /'
echo "on aesir: hourly; the first run sets its cursors (old receipts are not replayed) and sends today's summary mail"
echo "on aesir: within the hour Telegram says 'all well' or what is wrong; the ledger: sudo -u geodineum-comms -g adm /opt/geodineum/Geodineum-COMMS/target/release/geodineum-comms ledger <site>"
