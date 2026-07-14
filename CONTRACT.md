# Geodineum-COMMS — Integration Contract

Notification daemon that consumes per-site ValKey comms streams
(`{site_id}:gnode:comms:{env}`) and dispatches messages via email / Telegram /
SMS, with non-production side-effect gating. To have a message delivered you
`XADD` it to that stream in the format below. This file is the authoritative,
human-readable contract; print it any time with:

    geodineum comms contract

---

## 1. PROVIDES

Interfaces other components may rely on.

### 1.1 Outbound notification dispatch (stream consumer)

- **Consumes**: `XREADGROUP` from `{site_id}:gnode:comms:{env}`, consumer group
  **`geodineum_comms_dispatch`** (created per stream).
  `src/consumer/stream_reader.rs`
- **Produces**: `XACK` on terminal outcomes (`sent`, `skipped`, `failed`) — but
  **only after SQLite archival succeeds** (durable write fence).
  `src/consumer/stream_reader.rs`
- If SQLite archival fails, the entry stays pending and is re-claimed via
  `XAUTOCLAIM` after an idle-ms timeout.
- **Post-ACK channel retry**: a channel-level failure after ACK is retried by
  the daemon — the entry is re-fetched from stream history (`XRANGE` by id;
  entries survive XACK until trimmed) and **only the failed channels** are
  re-dispatched, with routing rules, per-recipient filters and the DTAP gate
  re-applied. Exponential backoff `retry.base_delay_secs` (30s) → `max_delay_secs`
  (1h), up to `max_attempts` (5). Success flips the SQLite row to `sent`; an
  entry trimmed from stream history abandons the retry with a warning.
  `src/main.rs`, `src/consumer/stream_reader.rs`,
  `src/router/dispatcher.rs`
- **Periodic site discovery**: streams are discovered at startup and
  re-discovered every `consumer.discovery_interval_secs` (default 60; `0` =
  startup-only), so sites onboarded after daemon start are consumed without a
  restart. `src/consumer/stream_reader.rs`, `src/config.rs`

### 1.2 Inbound operator replies (stream producer)

- Writes to `{site_id}:gnode:comms:inbound:{env}` when Telegram updates arrive.
  Routes operator commands back to the originating component via
  `metadata.callback_stream` from the original outbound message.
  `src/inbound/telegram_receiver.rs`, `src/main.rs`

### 1.3 Workflow dispatch (stream producer)

- Writes structured workflow requests to `{site_id}:gnode:comms:workflows:{env}`
  when an operator triggers `UPDATE` / `BACKUP` / `LOCKDOWN` / etc. via a Telegram
  reply. `config/schemas/workflow_dispatch.yaml`, `src/inbound/workflow_dispatch.rs`

### 1.4 Per-site message database (file)

- `/var/lib/geodineum-comms/{site_id}/messages.db` — SQLite, schema auto-created
  idempotently. `src/persistence/store.rs`

### 1.5 CLI commands

| Command | Role | Evidence |
|---------|------|----------|
| `geodineum-comms start` | Daemon entry point. Reads `{site_id}:gnode:comms:{--environment}` (default `production`), dispatches, archives to SQLite. Polls Telegram inbound if `TELEGRAM_BOT_TOKEN` set. | `src/config.rs`, `src/main.rs::main()` |
| `geodineum-comms test --site-id S --channel email\|telegram\|sms\|all` | Validates channel config for a site without sending. | `src/config.rs` |
| `geodineum-comms messages [--site-id] [--status] [--message-type] [--limit] [--format table\|json\|csv]` | Lists archived messages from SQLite with optional filters. | `src/config.rs` |
| `geodineum-comms sites` | Lists registered sites discovered from `{*}:gnode:comms:{env}` keys. | `src/config.rs` |
| `geodineum-comms encrypt --value STR` | **Stub** — prints a masked placeholder; no encryption is implemented. | `src/main.rs` |
| `geodineum-comms retry --site-id S --message-id MSG_ID` | Flips a `failed`/`partial_sent` archive row back to `pending` (SQLite status only; live re-sends happen via the daemon's retry loop, §1.1). | `src/cli/mod.rs` |
| `geodineum-comms cleanup [--site-id] [--max-age-days N] [--delete-spam] [--dry-run]` | Runs retention cleanup against SQLite per site. | `src/config.rs` |

`stop` and `status` are stubs that print a pointer to the systemd unit
(`src/main.rs`).

### 1.6 Internal API surface (for callers within the daemon)

- `parse_message(stream_key: &str, entry: &StreamEntry) -> Result<CommsMessage>`
  — parses `XREADGROUP` entry fields into a typed `CommsMessage`, resolving
  environment from the stream-key suffix or the explicit field.
  `src/consumer/stream_reader.rs`
- `dispatch(message: &CommsMessage) -> Result<DispatchResult>` — routes to
  enabled channels, applies priority/type filters per-recipient, enforces the
  non-production side-effect gate at `dispatch_to_channel`.
  `src/router/dispatcher.rs`
- `dispatch_channels(message, only: &[String]) -> Result<DispatchResult>` — the
  retry path: same pipeline narrowed to the given (previously failed) channel
  set; all filters and the DTAP gate re-apply. `src/router/dispatcher.rs`
- `send(message, recipient, config) -> Result<SendResult>` — `NotificationChannel`
  trait method. Email via lettre SMTP, Telegram via Bot API, SMS via Twilio REST.
  Returns a provider-specific message id on success.
  `src/channels/channel.rs`

---

## 2. CONSUMES / REQUIRES

What COMMS needs, and from which component.

| Input | Where / format | From component |
|-------|----------------|---------------|
| Outbound comms stream | `{site_id}:gnode:comms:{env}` (see §4 field table) | gNode, PHP services via gNode-Client |
| Site settings | ValKey **string** key `{site_id}:comms:config` — single JSON `SiteSettings` object | gCore admin UI / `geodineum` CLI (via gNode settings API) |
| DTAP environment schema | `development · testing · staging · acceptance · production`; only `production` permits real sends | gNode daemon config (`dtap_schema.yaml`) |
| SMTP credentials | Per-site host/port/user/password from `{site_id}:comms:config` → `channels.email.config`; TLS/STARTTLS. Stored as plain values guarded by the ValKey ACL (the `encrypt` verb is a stub, §1.5) | gCore admin (per-site settings) |
| Telegram Bot API token | **Plain string from `TELEGRAM_BOT_TOKEN` env var** (NOT from config file); forms `api_base = https://api.telegram.org/bot{token}` | `/etc/geodineum/credentials/` or env var |
| Twilio credentials | Per-site `account_sid`, `auth_token` from `{site_id}:comms:config` → `channels.sms.config`; ISO phone numbers with country code. Same plain-value storage caveat as SMTP | gCore admin (per-site settings) |
| Daemon spam blocklists | `spam_filter.keywords_blocklist` / `ip_blocklist` in `config/default.yaml` — **additive** extensions over the curated built-in lists; active only when `spam_filter.enabled: true` (default `false`). A spam hit is archived `status=spam` and ACK'd, never dispatched | operator config |
| gNode site registry | `gnode:site:{site_id}:meta` — scanned as a secondary site-discovery fallback when no `{*}:gnode:comms:{env}` stream exists for a site | gNode daemon |

Evidence: `src/settings/store.rs`, `src/settings/models.rs`,
`src/dtap.rs`, `src/channels/email/smtp.rs`, `src/config.rs`,
`src/inbound/telegram_receiver.rs`, `src/channels/sms/twilio.rs`,
`src/consumer/site_discovery.rs`, `src/main.rs`,
`src/filters/spam.rs`.

---

## 3. DTAP environments & the non-prod send gate

Tiers (canonical: `gNode/daemon/config/dtap_schema.yaml`):

    development · testing · staging · acceptance · production

**Only `production` permits real sends.** A message whose resolved
`environment` is anything else is **logged but not delivered** — the daemon
emits a `NON-PROD DRY-RUN` line naming the channel, recipient, and subject (body
at debug level) so non-prod notifications stay debuggable, but fires no real
email/SMS/Telegram.

- **Environment resolution priority**: message `environment` field (explicit) →
  `environment_from_stream_key(stream)` → fallback `"unknown"`.
- `is_production(env)` returns true **only** for a case-insensitive `production`
  match (`src/dtap.rs`). Fail-safe: unknown/missing → non-production.
- The gate is applied at `dispatcher.dispatch_to_channel`. To make a
  non-production daemon deliver for real, start it with `--allow-nonprod-send`
  (env `ALLOW_NONPROD_SEND=true`).

---

## 4. Wire formats

### 4.1 Outbound: `{site_id}:gnode:comms:{env}`

- **Braces are literal** — `{...}` is a ValKey Cluster hash-tag so every per-site
  key lands in the same slot. Write `{example_com}:gnode:comms:production`, not
  `example_com:gnode:comms:production`.
- `site_id` is derived **from the stream key**, not the body. It is validated
  post-brace-strip against `^[a-z0-9_-]{1,64}$`
  (`src/consumer/stream_reader.rs`).
- `env` is the DTAP tier (§3); the daemon's `--environment` filter (default
  `production`) selects which streams it reads.
- Max field size **64 KiB** per field (`parse_field_safely`,
  `src/consumer/stream_reader.rs`).

Scalar fields are plain strings. The four structured fields (`sender`,
`content`, `metadata`, `dispatch`) are **JSON-encoded strings** (not nested
objects).

| Field | Req | Type | Notes |
|-------|-----|------|-------|
| `id` | rec | string | Idempotency key; use a UUID. Defaults to the stream entry id if omitted. |
| `type` | rec | string | `contact` \| `alert` \| `error` \| `test` \| `system`. Not validated against an enum — any string is stored. (See §7 enum drift.) |
| `timestamp` | rec | string | ISO-8601 (`date -Iseconds`). Defaults to now. |
| `site_id` | opt | string | Informational only — the daemon uses the **stream key**. Include it for readability. |
| `environment` | rec | string | DTAP tier of the originating site (§3). Drives the non-prod send gate. **Recommended, not enforced**: falls back to the stream-key suffix, then `"unknown"`, if omitted — always stamp it explicitly. (See §7.) |
| `priority` | opt | int 1–5 | `1`=critical … `5`=low. Default `3`. Recipients can filter on a minimum priority. |
| `sender` | opt | JSON string | `{"name","email","phone","ip","user_agent"}` — all optional. |
| `content` | rec | JSON string | `{"subject","body","attachments":{}}`. **Has a fallback**: if absent/unparseable, the daemon synthesizes content from flat `subject` / `body` (or `message`) fields (Tera templates are then skipped). (See §7.) |
| `metadata` | opt | JSON string | Free-form object, e.g. `{"form_type","source_url","callback_stream","reply_options"}`. |
| `dispatch` | opt | JSON string | `{"channels":["email"],"status":"pending","attempts":0}`. `channels` selects which channels to use; if omitted the site's routing rules / all enabled channels apply. `status` is not enum-validated on the wire. **Never send an empty `channels` array** — it falls through to all enabled channels and would dispatch. Use a sentinel channel (below) to record-without-sending. |

Channels: `email`, `telegram`, `sms`. Sentinel (record-only, no outbound send —
message still lands on the stream and archives as skipped): `record`, `log`,
`none`. Per-site channel config + recipients live
in the site settings (`{site_id}:comms:config`) — credentials are never sent in
the message.

**Control message `settings.reload`** — a message with type=`settings.reload` is
a per-site settings-cache-invalidation signal, not a deliverable notification.
Producer: gNode-Client, emitted on every settings save/delete. The daemon
invalidates that site's in-memory settings cache and ACKs; it is never dispatched
to a channel.

### 4.2 `XACK`

    XACK {site_id}:gnode:comms:{env} geodineum_comms_dispatch <entry_id>

Fired **only after SQLite archive succeeds** (`stream_reader.rs`).

### 4.3 Inbound replies: `{site_id}:gnode:comms:inbound:{env}`

    XADD {site_id}:gnode:comms:inbound:{env} * \
      command=<QUARANTINE|DISMISS|RETRY|UPDATE> context_id=<ctx-ID> \
      component=<target-component> operator_id=<Telegram-user-ID> \
      operator_name=<name> channel_source=telegram ts=<ISO-8601>

Written by `TelegramReceiver` when an operator replies to an outbound alert.
Routed back to the originating component via `metadata.callback_stream`.

### 4.4 Workflow dispatch: `{site_id}:gnode:comms:workflows:{env}`

    XADD {site_id}:gnode:comms:workflows:{env} * \
      execution_id=<wf-type-timestamp> \
      workflow_id=<rolling_update|backup|lockdown|unlock|restart_service|deploy> \
      description=<text> params=<JSON-string> \
      operator_id=<ID> operator_name=<name> \
      status=<pending|running|completed|failed|cancelled> ts=<ISO-8601>

### 4.5 Site settings: `{site_id}:comms:config`

A ValKey **string** value holding a single JSON-encoded `SiteSettings` object
(not a hash):

    { site_id, enabled (bool, default true),
      channels { email?: ChannelConfig, telegram?: ChannelConfig, sms?: ChannelConfig },
      routing_rules [{ type, channels, priority_override? }],
      rate_limits { channel: RateLimit },
      filters { spam_enabled, spam_action, keywords_blocklist,
                ip_blocklist, email_blocklist },
      retry { max_attempts, base_delay_secs, max_delay_secs } }

    ChannelConfig { enabled, config {},
      recipients [{ address {key:val}, types [str], min_priority (int, default 5) }],
      rate_limit? { max_requests, window_secs } }

### 4.6 SQLite schema (per site)

`{GEODINEUM_COMMS_DATA_DIR}/{site_id}/messages.db`:

- `TABLE messages` (`id` TEXT PK, `stream_id`, `site_id`, `environment`,
  `message_type`, `priority` INT, `sender_json`, `content_json`,
  `metadata_json`, `status` TEXT [`pending`|`processing`|`sent`|`failed`|`spam`],
  `attempts` INT, `channel_results_json` TEXT, `spam_score` REAL,
  `received_at`, `updated_at`, `completed_at`)
- `TABLE channel_results` (`id` INT PK AUTO, `message_id` FK, `channel`,
  `success` INT [0|1], `provider_id`, `error`, `sent_at`)
- Indexes on `status`, `type`, `received_at DESC`, `environment`.

---

## 5. Public types

    CommsMessage { id, message_type, timestamp, site_id, priority: u8,
                   sender: Option<SenderInfo>, content: MessageContent,
                   metadata: HashMap<String,Value>, environment: String,
                   dispatch: Option<DispatchInfo> }
    SenderInfo { name?, email?, phone?, user_agent?, ip? }
    MessageContent { subject?, body?, attachments: HashMap<String,Value> }
    DispatchInfo { channels: Vec<String>, status: DispatchStatus,
                   attempts: u32, last_attempt?, next_retry? }
    DispatchStatus = Pending | Processing | Sent | Failed | Spam
    SiteSettings { site_id, enabled, channels: ChannelSettings,
                   routing_rules, rate_limits, filters, retry }
    ChannelConfig { enabled, config, recipients: Vec<RecipientConfig>, rate_limit? }
    RecipientConfig { address: HashMap<String,String>, types: Vec<String>,
                      min_priority: u8 }
    SendResult { success, provider_id?, metadata: HashMap<String,String> }
    DispatchResult { message_id, successful_channels: Vec<String>,
                     failed_channels: Vec<DispatchError>,
                     skipped_channels: Vec<String> }

---

## 6. Examples

### 6.1 Raw `redis-cli` / `valkey-cli`

    valkey-cli -p 47445 --user geodineum_comms XADD \
      '{example_com}:gnode:comms:production' '*' \
      id "$(uuidgen)" \
      type contact \
      timestamp "$(date -Iseconds)" \
      site_id example_com \
      environment production \
      priority 3 \
      sender  '{"name":"Jane","email":"jane@example.com","ip":"203.0.113.7"}' \
      content '{"subject":"Hello","body":"Message body."}' \
      dispatch '{"channels":["email"]}'

### 6.2 PHP (the supported path)

Use gNode-Client; it builds the key and fields for you and stamps `environment`:

    $gNodeClient->queueContactForm($name, $email, $subject, $message, [
        'source_url' => $url,
        'ip'         => $clientIp,
    ]);

    // …or the general form:
    $gNodeClient->queueCommsMessage(
        'alert',                                   // type
        ['name' => 'System', 'email' => 'ops@…'],  // sender
        ['subject' => 'Disk full', 'body' => '…'], // content
        ['form_type' => 'alert'],                  // metadata
        1,                                         // priority (critical)
        ['email', 'telegram']                      // channels
    );

### 6.3 Two-way operator automation

Set `metadata.reply_options = ["QUARANTINE","DISMISS","RETRY"]` and
`metadata.callback_stream = "alerting:gnode:comms:inbound:production"` on the
outbound alert. An operator button press → `callback_query` → `TelegramReceiver`
writes the command to `callback_stream` → the originating daemon polls and
executes it. **No automatic correlation if `callback_stream` is missing**, and
the daemon does **not** validate that `callback_stream` matches the canonical
inbound pattern — a malformed value is written as-is (§7).

---

## 7. Adherence — cross-component reconciliation

The deployed PHP producers (gNode-Client, gTemplate, and gCore-based child themes) always send the
optional fields and the consumer never validates enums, so none of these ever
broke live interop. The documentation-vs-code drifts were reconciled 2026-06-22:

- **RESOLVED — `environment` / `content` required-vs-optional.** Both are marked
  **rec** (recommended) in §4.1, matching the consumer's lenient fallback
  (`environment` → stream-key suffix → `"unknown"`, `stream_reader.rs`;
  `content` → flat `subject`/`body`/`message`, `stream_reader.rs`, Tera
  skipped on that path). Producers always stamp both.
- **RESOLVED — message-type enum.** All three surfaces now agree on
  `contact|alert|error|test|system`: this doc (§4.1), `config/schemas/outbound_alert.yaml`,
  and the gNode-Client `queueCommsMessage` docblock (the stray `contact-form`/`custom`
  were removed). `type` remains free-form on the wire — `parse_message` stores any
  string; `test` is env-filtered, `system` never dispatched. The reserved
  type=`settings.reload` (§4.1) is a control signal — settings-cache invalidation
  only, ACK'd and never dispatched.
- **RESOLVED — data-dir default.** `config_schema.yaml` now defaults to
  `/var/lib/geodineum-comms`, matching `main.rs::get_data_dir` and the unit's
  `ReadWritePaths`.

Remaining low-severity code-hardening notes (no contract drift, behaviour is correct):

1. **`dispatch.status` / skip-marker is a string, not an enum.** The non-prod skip
   keys off the literal `"nonprod_dry_run"` (`dispatcher.rs`) rather than a
   first-class variant — fragile if that string diverges between its two call
   sites. Consider a `CommsError` variant.
2. **`callback_stream` is not validated.** An outbound `metadata.callback_stream`
   is written as-is; a malformed value could route an inbound reply to the wrong
   stream. Consider validating it against `{component}:gnode:comms:inbound:{env}`.

7. **Archived `environment` may diverge from routing `environment`.** The SQLite
   archive stores `environment` from `metadata.environment` (`store.rs`), but
   routing decisions use the top-level `message.environment` field. If they
   differ, the audit record may not match the value that drove the gate.

8. **Inbound subsystem contract is incomplete.** `ConversationState` /
   `InferenceChainConfig` are loaded from ValKey (`{site_id}:comms:conversations:*`,
   `{site_id}:inference:history:*`) with **no wire-format spec** — the inbound
   (two-way Telegram) subsystem is beta for Ch.1 and undocumented here.
