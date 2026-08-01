# Geodineum-COMMS :: CONTRACT primer (SCN)

> one-line: SCN primer — TRUTH = code on disk, this file is a point-in-time compression. Companion: CONTRACT.md (authoritative).

## ::ROLE

Notification daemon. gNode=Sun, ValKey=common backend. Stateless/state-aware: consumer-group resumes from last `XACK`, restarts safe. Consumes per-site comms streams `{site_id}:gnode:comms:{env}` → dispatches email|telegram|sms → archives SQLite → `XACK`. Non-production = side-effect-gated by default (fail-safe).

## ::ANCHOR

- key types: `CommsMessage` · `MessageContent` · `SenderInfo` · `DispatchInfo`/`DispatchStatus{Pending|Processing|Sent|Failed|Spam}` · `SiteSettings` · `ChannelConfig` · `RecipientConfig` · `SendResult` · `DispatchResult`
- stream keys (braces LITERAL = Cluster hash-tag):
  - IN  `{site_id}:gnode:comms:{env}` — outbound dispatch queue (consume)
  - OUT `{site_id}:gnode:comms:inbound:{env}` — operator replies (produce)
  - OUT `{site_id}:gnode:comms:workflows:{env}` — workflow requests (produce)
  - CFG `{site_id}:comms:config` — ValKey STRING (single JSON `SiteSettings`, NOT a hash)
  - DISCOVER `gnode:site:{site_id}:meta` — fallback site registry scan
- consumer group: `geodineum_comms_dispatch`
- parse: `parse_message(stream_key,entry)->CommsMessage` `stream_reader.rs`
- read/ack: `XREADGROUP` `stream_reader.rs` · `XACK` `stream_reader.rs`
- route+gate: `dispatch(msg)->DispatchResult` `dispatcher.rs` · retry-narrowed `dispatch_channels(msg,only)` `dispatcher.rs` · gate@`dispatch_to_channel` skip-marker string `"nonprod_dry_run"` `dispatcher.rs`
- channel trait: `send(msg,recipient,config)->SendResult` `channels/channel.rs`
- dtap: `is_production(env)` case-insensitive `=="production"` ONLY `dtap.rs`
- validation: site_id `^[a-z0-9_-]{1,64}$` `stream_reader.rs` · field cap 64KiB `parse_field_safely`
- db path: `/var/lib/geodineum-comms/{site_id}/messages.db` `store.rs` (binary default `main.rs`)
- env merge: `TELEGRAM_BOT_TOKEN` `config.rs`
- retry: post-ACK re-dispatch loop `main.rs` · re-fetch `fetch_message` XRANGE `stream_reader.rs` · CLI `retry` verb = SQLite status flip ONLY `cli/mod.rs`
- discovery: periodic `maybe_rediscover` `stream_reader.rs` every `discovery_interval_secs` (default 60, 0=startup-only `config.rs`)
- spam: `SpamFilter::with_extra_blocklists` YAML-additive over built-ins `spam.rs` wired `main.rs` (enabled:false default)
- CLI enum: `config.rs` (start|stop*|status*|test|encrypt*|sites|messages|stats|message|retry|cleanup|db-stats; *=print-only stub `main.rs`)
- entry: `main.rs`, main loop spawns StreamConsumer + RetryManager(XAUTOCLAIM + post-ACK re-dispatch) + TelegramReceiver + response_poller
- dispatch field: producer-minimal (ALL fields defaulted; `{}` valid); `reply_markup` → telegram inline keyboard verbatim, other channels ignore. Buttons answered via answerCallbackQuery AFTER stream write (ack = recorded, not received).
- liveness: SETEX `{ns}:gnode:heartbeat:{env}:comms:{node}` 120 (node = short hostname; CONTRACTS/heartbeat.md)

## ::ARCHITECTURE

Rust + tokio 1.35 async (full). clap 4.4 CLI. Modules:
- `consumer/` — `SiteDiscovery` (SCAN pattern) + `StreamConsumer` (`XREADGROUP`)
- `router/` — `MessageDispatcher` (routing_rules + per-recipient type/priority gate + DTAP side-effect gate)
- `channels/` — `NotificationChannel` trait → Email(lettre 0.11 SMTP) | Telegram(reqwest 0.12 Bot API) | SMS(Twilio REST)
- `persistence/` — `MessageStore` r2d2 SQLite pool, per-site isolation
- `settings/` — `SettingsStore` ValKey-backed + in-mem RwLock cache
- `inbound/` — `TelegramReceiver` long-poll getUpdates + CommandRouter + ConversationState + SPR engine + `WorkflowDispatcher` + response_poller [BETA, undocumented schema]
- `dtap/` — is_production + environment_from_stream_key
- `templates/` — Tera per-channel · `cli/` — output formatters
KEY design: durable-write-fence (`XACK` ONLY after SQLite ok → crash-safe via group rebalance) · fail-safe DTAP (unknown/missing env → non-prod → no real send) · per-site `DispatchRateLimiter` sliding-window 100/60s, MAX_TRACKED_SITES=1024 hard-cap (anti-OOM) · brace-literal hash-tags for Cluster co-location · exp backoff 30s→1h · post-ACK channel retry (XRANGE re-fetch from stream history, failed-channels-only re-dispatch, success→archive `sent`, trimmed→abandon+warn) · periodic site re-discovery (discovery_interval_secs=60 default, 0=startup-only).
systemd: Type=simple User=geodineum-comms After=valkey-gnode+gnode-daemon, Restart=on-failure RestartSec=5, ProtectSystem=strict ProtectHome=read-only PrivateTmp=true, ReadWritePaths=.gnode logs + /var/lib/geodineum-comms.

## ::IO

IN ← `{site_id}:gnode:comms:{env}` fields: scalar `id`/`type`/`timestamp`/`site_id`/`environment`/`priority` + JSON-string `sender`/`content`/`metadata`/`dispatch`. site_id from KEY not body. env resolution: msg.environment → stream-key-suffix → "unknown".
IN ← `{site_id}:comms:config` (JSON SiteSettings) · `gnode:site:*:meta` (discovery) · SMTP creds (config) · Twilio creds (config) · `TELEGRAM_BOT_TOKEN` (ENV, not config).
OUT → channels: lettre SMTP | Telegram sendMessage/answerCallbackQuery/editMessageText | Twilio SendSMS. Sentinel channels `record`/`log`/`none` = record-only (skipped, no send, still on stream); empty `dispatch.channels` = ALL enabled (never emit empty).
OUT → `{site_id}:gnode:comms:inbound:{env}` (operator reply: command/context_id/component/operator_id/ts) routed via `metadata.callback_stream`.
OUT → `{site_id}:gnode:comms:workflows:{env}` (execution_id/workflow_id/params/status/ts).
OUT → SQLite `messages`+`channel_results` tables (write-fence before XACK).
OUT → `XACK ... geodineum_comms_dispatch <id>` post-archive.

## ::CONTRACT

PROVIDES:
- stream-consumer on `{site_id}:gnode:comms:{env}` grp `geodineum_comms_dispatch`; XACK post-SQLite.
- inbound-reply + workflow-dispatch producers (see ::IO).
- per-site SQLite archive file.
- CLI: start|test|messages|stats|message|sites|retry|cleanup|db-stats (stop|status|encrypt = print-only stubs; retry verb = archive-status flip, real re-sends = daemon loop).
CONSUMES:
- outbound msg format (field table → CONTRACT.md §4.1).
- `{site_id}:comms:config` JSON SiteSettings string.
- DTAP schema (dev|test|staging|acceptance|prod; prod=only-real-send).
- SMTP/Twilio creds (per-site config) · Telegram token (ENV).

## ::USECASES

alert distribution (type=alert pri 1-2 → multi-channel) · contact-form ingestion → site owner + SQLite · two-way operator automation (reply_options+callback_stream → QUARANTINE/etc) · non-prod dry-run testing (logs, no send) · inbound Telegram chat (SPR+LLM) [beta] · workflow triggering (UPDATE→workflows stream→Celery) · audit/compliance (every attempt persisted pre-XACK) · retry mgmt (pre-ACK: XAUTOCLAIM re-claim; post-ACK: XRANGE re-fetch + failed-channels-only re-dispatch, exp backoff 30s→1h, max 5) · rate limiting (per-site window) · spam filtering (curated built-ins + additive YAML blocklists → spam_score/status, archived+ACKed, never dispatched).

## ::LIMITATIONS

- `environment` + `content`: CONTRACT historically "required" but code OPTIONAL with fallback (env→stream-suffix→"unknown"; content→flat subject/body/message, Tera skipped). Drift, not live break.
- type enum reconciled to `contact|alert|error|test|system` across CONTRACT/`outbound_alert.yaml`/client docblock (stray `custom`,`contact-form` removed 2026-06-22); `parse_message` still does NOT validate → any string stored. Reserved control type `settings.reload` (producer gNode-Client on every settings save/delete) = per-site settings-cache invalidation only: daemon drops cached settings + ACKs, NEVER dispatched to a channel.
- DTAP gate keys off hardcoded string `"nonprod_dry_run"` (not enum) — fragile if one site changes, gating breaks SILENTLY.
- `callback_stream` UNVALIDATED → malformed value written as-is, can misroute.
- archived env (from metadata.environment, `store.rs`) MAY diverge from routing env (top-level field) → audit confusion.
- data-dir default: `/var/lib/geodineum-comms` (schema + binary + unit aligned 2026-06-22).
- inbound subsystem BETA; ConversationState/inference keys `{site_id}:comms:conversations:*` undocumented, no wire-spec.
- NO per-daemon admin UI/API: operator surface = wp-admin (Geodineum→Comms) + `geodineum comms` CLI; former src/api retired (git history).
- CLI stop|status|encrypt = print-only stubs (`main.rs`); NO encryption anywhere → per-site creds stored plain in ValKey (ACL-gated).
- SMTP no conn-pool (fresh transport/send) · Telegram long-poll only (no webhook) · single-env per daemon (`--environment` one tier) · SQLite no at-rest encryption (relies on LUKS+perms) · DTAP tiers hardcoded.

## ::GRAPH

DEPENDS_ON: ValKey (valkey-gnode.service :47445, ACL user `geodineum_comms`) · gNode daemon (DTAP schema, site meta) · Telegram Bot API + Twilio REST + arbitrary SMTP · Geodineum installer (systemd unit, `/etc/geodineum/credentials/valkey_comms.password`, env file, geodeploy.yaml).
PROVIDES_TO: originating components via inbound-reply stream (callback_stream) · Celery/automation via workflows stream · operators via SQLite archive + CLI.
ADHERES_TO: comms-message wire format (producers gNode-Client `queueCommsMessage`/`queueContactForm`, child-theme+gTemplate direct-XADD — ALL verified ADHERES) · DTAP schema of gNode.
ISOLATED_FROM: gMath · gNode command/health streams (does not parse t/c/p/ss/sn or health metrics) · signed-extension scheme (gNode-CMS).

## ::LATENT

- "XACK only after SQLite — durable write fence, crash-safe via consumer-group rebalance"
- "fail-safe DTAP gate: env→stream-suffix→unknown, is_production=='production' only, nonprod=dry-run-no-send"
- "brace-literal {site_id} hash-tag co-locates per-site data in one Cluster slot"
- "site_id from STREAM KEY not body, regex ^[a-z0-9_-]{1,64}$, 64KiB field cap"
- "stream producers — gNode-Client + gTemplate + child themes — all stamp top-level scalar environment, verified aligned"
- "nonprod_dry_run hardcoded skip-marker string, not an enum — fragile gate coupling"
- "TELEGRAM_BOT_TOKEN from ENV not config; inbound two-way Telegram subsystem is BETA, undocumented schema"
- "CONTRACT says required, code says optional-with-fallback: environment + content drift"
- "post-ACK retry = XRANGE stream-history re-fetch, failed channels ONLY, all filters re-apply; trim = abandon"
- "YAML spam blocklists ADDITIVE over curated built-ins — config can only strengthen, never weaken"
