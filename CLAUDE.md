# GSD-COMMS: Notification Daemon
## LLM-Native Architecture Reference (SCN/SPR Format)

```yaml
@PATHS:
  project: /opt/geodineum/GSD-COMMS    # This project
  gsd: /opt/geodineum/GSD              # Main GSD daemon (production)
  gsd_dev: /opt/GSD                    # GSD development branch
  gsd_client: /opt/geodineum/GSD-Client # PHP client library
  gcore: ~/gh/gCore                    # gCore framework (admin dashboard)
  gcore_comms: ~/gh/gCore/Modules/Comms # Dashboard module
```

```yaml
@PRIME: notification-daemon|multi-channel|valkey-streams|rust-async|
        email-smtp|telegram-bot|sms-api|per-site-config|multi-tenant|
        gcore-dashboard|wordpress-admin|template-engine|retry-backoff|
        spam-filter|rate-limiting|idempotency|modular-channels|json-api
```

---

## §0 SYSTEM IDENTITY

```yaml
@WHAT: notification-daemon + json-api + multi-channel-dispatcher
@WHY: GSD-comms-streams-need-processing→dispatch-to-email/telegram/sms→user-alerts
@HOW: XREADGROUP(comms-streams)→route(type+priority+settings)→dispatch(channels)
@STACK: Rust(daemon) + Axum(API) + gCore/PHP(dashboard) + ValKey(storage)
@PROTOCOL: RESP3-consumer-groups|SMTP|Telegram-Bot-API|Twilio-REST
@INTEGRATION: reads-from-GSD-comms-streams|uses-GSD-valkey-instance|gcore-admin-module
@STATUS: implementation-phase(2026-01-10)
```

---

## §1 CRITICAL CONSTRAINTS

```yaml
@SUPPRESS: # NEVER do these
  - redis-*(use:valkey-*)
  - valkey-cli --pass(use:REDISCLI_AUTH=...)
  - blocking-io-in-async(use:tokio-native)
  - hardcoded-credentials(use:.env|secrets-manager)
  - .unwrap()-in-production(use:.map_err()?|.unwrap_or_default())
  - panic!-in-production(use:Result<T,E>-propagation)
  - send-without-idempotency-check(duplicate-prevention)

@REQUIRE: # ALWAYS do these
  - ACL-user-for-comms(separate-from-gsd_daemon)
  - per-site-settings-isolation({site_id}:comms:config)
  - Result<T,E>-propagation(rust-error-handling)
  - rate-limiting-per-channel(prevent-abuse)
  - template-based-messages(no-hardcoded-content)
  - retry-with-exponential-backoff(transient-failures)
  - idempotency-key-tracking(prevent-duplicates)
```

---

## §2 MESSAGE FORMAT (FROM GSD COMMS STREAM)

```yaml
@STREAM_KEY: {site_id}:gsd:comms:{environment}
@EXAMPLES:
  - staging_nierto_com:gsd:comms:production
  - nierto_com:gsd:comms:production

@MESSAGE_SCHEMA:
  stream_id: "1766066809145-0"  # ValKey stream entry ID
  fields:
    id: "uuid"                  # Unique message ID (idempotency key)
    type: "contact|contact-form|system|test|alert|error"
    timestamp: "ISO-8601"       # When message was created
    site_id: "site_name"        # Multi-tenant identifier
    priority: 1-5               # 1=critical, 5=low

    sender:                     # Who triggered the notification
      name: "string"
      email: "string"
      phone: "string?"
      user_agent: "string"
      ip: "string"

    content:                    # Notification content
      subject: "string"
      body: "string"
      attachments: {}           # Future: file attachments

    metadata:                   # Context information
      form_type: "contact|newsletter|support"
      source_url: "string"
      face_id: number           # WordPress face ID
      environment: "production|staging"

    dispatch:                   # Dispatch status (updated by GSD-COMMS)
      channels: ["email", "telegram", "sms"]
      status: "pending|processing|sent|failed|spam"
      attempts: number
      last_attempt: "timestamp|null"
      next_retry: "timestamp|null"

@MESSAGE_TYPES:
  contact: user-submitted-contact-form
  contact-form: alias-for-contact
  system: system-generated-notification
  test: test-message (skip-in-production)
  alert: high-priority-system-alert
  error: error-notification (high-priority)

@PRIORITY_LEVELS:
  1: critical (immediate, bypass-rate-limits)
  2: high (within-5-minutes)
  3: normal (within-15-minutes, default)
  4: low (within-1-hour)
  5: bulk (batched, off-peak)
```

---

## §3 ARCHITECTURE

```yaml
@CORE_FLOW:
  GSD-client→XADD(comms-stream)→GSD-COMMS-XREADGROUP→
  parse→validate→check-idempotency→load-site-settings→
  route(channels)→render-templates→dispatch→update-status→XACK

@COMPONENTS:
  StreamConsumer:      # Connects to ValKey, XREADGROUP on all site comms streams
    discovery: dynamic-site-discovery-from-GSD
    consumer_group: gsd_comms_dispatch
    consumer_name: gsd_comms_{node_id}

  MessageRouter:       # Routes messages to appropriate channels
    routing_rules: type→priority→site_settings→channel_selection

  ChannelProviders:    # Pluggable notification channels
    email: lettre(async-smtp)
    telegram: teloxide|reqwest(bot-api)
    sms: reqwest(twilio-rest-api)
    # Future: discord, slack, webhook, push

  SettingsManager:     # Per-site configuration
    storage: {site_id}:comms:config
    cache: in-memory-with-invalidation

  TemplateEngine:      # Message templates
    engine: tera|minijinja
    templates: per-channel-per-type

  RetryManager:        # Handles transient failures
    strategy: exponential-backoff(base:30s,max:1h,jitter:±20%)
    max_attempts: 5

  SpamFilter:          # Future: spam detection
    basic: keyword-blocklist|ip-blocklist
    advanced: ml-scoring (future)

  AdminAPI:            # Dashboard REST API
    framework: axum
    dashboard: htmx-server-rendered

@DATA_FLOW_DIAGRAM:
  ┌────────────────┐
  │  GSD Client    │
  │  (PHP/Any)     │
  └───────┬────────┘
          │ XADD
          ▼
  ┌────────────────┐
  │  ValKey        │
  │  Comms Stream  │
  └───────┬────────┘
          │ XREADGROUP
          ▼
  ┌────────────────────────────────────────────────┐
  │              GSD-COMMS Daemon                   │
  │  ┌─────────────┐    ┌─────────────────────┐   │
  │  │   Stream    │───▶│  Message Router     │   │
  │  │  Consumer   │    │  (type/priority/    │   │
  │  │             │    │   settings)         │   │
  │  └─────────────┘    └──────────┬──────────┘   │
  │                                │              │
  │  ┌─────────────┐    ┌──────────▼──────────┐   │
  │  │  Settings   │◀──▶│  Template Engine    │   │
  │  │  Manager    │    │  (per-channel)      │   │
  │  └─────────────┘    └──────────┬──────────┘   │
  │                                │              │
  │  ┌─────────────┬───────────────┼───────────┐  │
  │  │             │               │           │  │
  │  ▼             ▼               ▼           │  │
  │┌─────┐    ┌─────────┐    ┌─────────┐       │  │
  ││Email│    │Telegram │    │   SMS   │  ...  │  │
  ││SMTP │    │Bot API  │    │ Twilio  │       │  │
  │└──┬──┘    └────┬────┘    └────┬────┘       │  │
  │   │            │              │            │  │
  │   └────────────┴──────────────┘            │  │
  │                │                           │  │
  │  ┌─────────────▼───────────┐               │  │
  │  │     Retry Manager       │               │  │
  │  │  (failures → backoff)   │               │  │
  │  └─────────────────────────┘               │  │
  └────────────────────────────────────────────┘
          │
          │ Status Update (HSET)
          ▼
  ┌────────────────┐
  │  ValKey        │
  │  Message State │
  └────────────────┘
```

---

## §4 CHANNEL PROVIDERS

```yaml
@TRAIT_DEFINITION:
  NotificationChannel:
    fn name(&self) -> &str
    fn send(&self, message: &CommsMessage, config: &ChannelConfig) -> Result<SendResult>
    fn validate_config(&self, config: &ChannelConfig) -> Result<()>
    fn rate_limit(&self) -> RateLimit

@EMAIL_PROVIDER:
  library: lettre (0.11+)
  transport: AsyncSmtpTransport
  features:
    - connection-pooling (reuse-connections)
    - TLS/STARTTLS (secure-by-default)
    - DKIM-signing (optional)
    - HTML+plaintext (multipart)
  config_fields:
    smtp_host: string
    smtp_port: 587|465|25
    smtp_user: string
    smtp_pass: string (encrypted)
    from_email: string
    from_name: string
    reply_to: string?
  rate_limit: 100/minute (configurable)

@TELEGRAM_PROVIDER:
  library: reqwest (direct-bot-api) | teloxide (framework)
  transport: HTTPS-POST
  features:
    - markdown-v2-formatting
    - channel-posting
    - inline-keyboards (optional)
    - file-attachments (optional)
  config_fields:
    bot_token: string (encrypted)
    chat_id: string (channel-id or user-id)
    parse_mode: MarkdownV2|HTML|None
    disable_notification: bool
  rate_limit: 30/second (Telegram-limit)

@SMS_PROVIDER:
  library: reqwest (twilio-rest-api)
  transport: HTTPS-POST
  features:
    - international-numbers
    - delivery-status-webhooks (future)
    - MMS-support (future)
  config_fields:
    provider: twilio|vonage
    account_sid: string (encrypted)
    auth_token: string (encrypted)
    from_number: string
    to_number: string
  rate_limit: 10/minute (cost-control)

@FUTURE_CHANNELS:
  discord: webhook-based
  slack: webhook-based
  webhook: generic-HTTP-POST
  push: firebase-fcm|apple-apns
```

---

## §5 SITE SETTINGS SCHEMA

```yaml
@STORAGE_KEY: {site_id}:comms:config

@SETTINGS_SCHEMA:
  site_id: string               # Must match stream site_id
  enabled: bool                 # Global on/off

  channels:                     # Per-channel configuration
    email:
      enabled: bool
      config:
        smtp_host: string
        smtp_port: int
        smtp_user: string
        smtp_pass: "encrypted:..."
        from_email: string
        from_name: string
        reply_to: string?
      recipients:               # Who receives notifications
        - email: "admin@site.com"
          types: ["all"]        # or ["contact", "alert"]
          min_priority: 3       # Only priority 1-3

    telegram:
      enabled: bool
      config:
        bot_token: "encrypted:..."
        chat_id: "-1001234567890"
        parse_mode: "MarkdownV2"
      recipients:
        - chat_id: "-1001234567890"
          types: ["alert", "error"]
          min_priority: 2

    sms:
      enabled: bool
      config:
        provider: "twilio"
        account_sid: "encrypted:..."
        auth_token: "encrypted:..."
        from_number: "+1234567890"
      recipients:
        - phone: "+1987654321"
          types: ["alert"]      # Only critical alerts
          min_priority: 1       # Priority 1 only

  routing_rules:                # Custom routing logic
    - type: "contact"
      channels: ["email"]
      priority_override: null
    - type: "alert"
      channels: ["email", "telegram", "sms"]
      priority_override: 1
    - type: "error"
      channels: ["email", "telegram"]
      priority_override: 2

  templates:                    # Per-type templates (optional override)
    contact:
      email:
        subject: "New contact from {{sender.name}}"
        body_template: "contact_email.html"
      telegram:
        body_template: "contact_telegram.md"

  rate_limits:                  # Per-channel rate limits
    email: 100/hour
    telegram: 60/hour
    sms: 10/hour

  filters:                      # Message filters
    spam:
      enabled: bool
      action: "reject|flag|quarantine"
      keywords_blocklist: ["viagra", "crypto"]
      ip_blocklist: ["1.2.3.4"]

  retry:                        # Retry configuration
    max_attempts: 5
    base_delay_secs: 30
    max_delay_secs: 3600
```

---

## §6 ADMIN DASHBOARD

```yaml
@FRAMEWORK: gCore (PHP) + WordPress Admin
@WHY_GCORE:
  - existing-admin-templates-and-security
  - valkey-integration-via-CacheManager
  - wordpress-native (same-tech-as-sites)
  - no-recompilation-for-ui-changes
  - ACL-handled-by-WordPress-roles

@GCORE_MODULE: ~/gh/gCore/Modules/Comms
  CommsManager.php:            # Core module
    - getSiteSettings(siteId)  # Read config from ValKey
    - saveSiteSettings(siteId) # Write config to ValKey
    - getRecentMessages(siteId)# Read from comms stream
    - getStats(siteId)         # Aggregate statistics
    - testChannel(siteId)      # Validate config
    - getDaemonStatus(siteId)  # Check consumer group

  Admin/CommsAdmin.php:        # WordPress integration
    - registerMenus()          # gCore submenu
    - renderDashboard()        # Main dashboard
    - renderSettings()         # Channel configuration
    - ajaxSaveSettings()       # AJAX endpoint
    - ajaxTestChannel()        # Test delivery
    - ajaxGetStats()           # Refresh stats

  Templates/dashboard.php:     # Dashboard UI
    - daemon-status-card
    - statistics-grid
    - recent-messages-table
    - channel-breakdown

  Templates/settings.php:      # Settings UI
    - global-enable-toggle
    - email-channel-config
    - telegram-channel-config
    - sms-channel-config
    - routing-rules-matrix
    - rate-limits
    - spam-filter-config
    - retry-settings

@WORDPRESS_PAGES:
  /wp-admin/admin.php?page=gcore-comms          # Dashboard
  /wp-admin/admin.php?page=gcore-comms-settings # Settings

@RUST_API_ENDPOINTS: # JSON API for internal use + gCore
  GET  /api/health              # Health check
  GET  /api/sites               # List configured sites
  GET  /api/sites/{id}          # Get site config
  PUT  /api/sites/{id}          # Update site config
  POST /api/sites/{id}/test     # Test channel delivery
  GET  /api/messages            # List messages (paginated)
  GET  /api/messages/{id}       # Get message detail
  POST /api/messages/{id}/retry # Manual retry
  GET  /api/stats               # Delivery statistics

@RUST_DASHBOARD: # Minimal status page at daemon port
  /dashboard:                   # Shows daemon running status
    - status-indicator          # green/red dot
    - basic-stats               # sites/pending/sent
    - link-to-wordpress-admin   # "Configure in WP Admin"
    - api-endpoints-reference   # developer info

@DATA_FLOW:
  WordPress-Admin→gCore-AJAX→ValKey(settings)
  gCore-AJAX→ValKey-XREVRANGE→message-history
  gCore-AJAX→ValKey-XINFO→daemon-status
  # Settings written by gCore are read by Rust daemon
  # Both use same ValKey keys: {site_id}:comms:config
```

---

## §7 CLI OPTIONS

```yaml
@INVOKE: ./target/release/gsd-comms [OPTIONS] [SUBCOMMAND]

@OPTIONS:
  --redis-host: ValKey host [default: 127.0.0.1]
  --redis-port: ValKey port [default: 47445]
  --redis-user: ACL username [default: gsd_comms]
  --redis-auth: ACL password
  --config: path to config file [default: config/default.yaml]
  --log-level: error|warn|info|debug|trace [default: info]
  --api-port: Dashboard API port [default: 8080]
  --api-bind: Dashboard bind address [default: 127.0.0.1]
  --workers: Number of worker threads [default: auto]
  --node-id: Unique node identifier [default: hostname]

@SUBCOMMANDS:
  start: Start the daemon
  stop: Stop the daemon
  status: Check daemon status
  test: Test notification channels
    --site-id: Site to test
    --channel: email|telegram|sms|all
  migrate: Run database migrations
  encrypt: Encrypt a secret value
    --value: Value to encrypt

@EXAMPLES:
  # Start daemon
  ./gsd-comms --redis-auth "$(cat .gsd/valkey_comms.password)" start

  # Test email channel for a site
  ./gsd-comms test --site-id staging_nierto_com --channel email

  # Encrypt a secret
  ./gsd-comms encrypt --value "my-api-key"
```

---

## §8 DEPENDENCIES

```yaml
@CORE:
  tokio: async-runtime (full-features)
  serde: serialization (json+yaml)
  serde_json: JSON handling
  config: configuration-management
  tracing: structured-logging
  tracing-subscriber: log-formatting

@VALKEY:
  redis: valkey-client (async)
  r2d2: connection-pooling (optional)

@WEB:
  axum: web-framework
  tower: middleware
  tower-http: http-utilities (cors, compression)

@EMAIL:
  lettre: smtp-client (async)

@TELEGRAM:
  teloxide: telegram-bot (optional)
  # OR reqwest for direct API

@HTTP_CLIENT:
  reqwest: http-client (for-twilio+telegram-api)

@TEMPLATES:
  tera: template-engine
  # OR minijinja (lighter)

@CRYPTO:
  ring: encryption (AES-GCM for secrets)
  base64: encoding

@UTILITIES:
  chrono: datetime
  uuid: unique-ids
  thiserror: error-handling
  anyhow: error-context
```

---

## §9 FILE STRUCTURE

```yaml
GSD-COMMS/
├── Cargo.toml
├── Cargo.lock
├── CLAUDE.md                     # This file (LLM context)
├── README.md                     # User documentation
├── .env.example                  # Environment template
├── .gitignore
│
├── config/
│   ├── default.yaml              # Default configuration
│   └── templates/                # Notification templates
│       ├── email/
│       │   ├── contact.html      # Contact form email
│       │   ├── alert.html        # Alert email
│       │   └── base.html         # Base layout
│       ├── telegram/
│       │   ├── contact.md        # Contact form message
│       │   └── alert.md          # Alert message
│       └── sms/
│           ├── contact.txt       # Contact form SMS
│           └── alert.txt         # Alert SMS
│
├── scripts/
│   ├── install-service.sh        # Systemd service installer
│   ├── setup.sh                  # Initial setup script
│   └── create-acl-user.sh        # Create ValKey ACL user
│
├── src/
│   ├── main.rs                   # Entry point
│   ├── lib.rs                    # Library exports
│   ├── config.rs                 # Configuration management
│   ├── error.rs                  # Error types
│   │
│   ├── consumer/                 # Stream consumer
│   │   ├── mod.rs
│   │   ├── stream_reader.rs      # XREADGROUP logic
│   │   └── site_discovery.rs     # Dynamic site discovery
│   │
│   ├── router/                   # Message routing
│   │   ├── mod.rs
│   │   └── dispatcher.rs         # Route to channels
│   │
│   ├── channels/                 # Notification channels
│   │   ├── mod.rs
│   │   ├── channel.rs            # NotificationChannel trait
│   │   ├── email/
│   │   │   ├── mod.rs
│   │   │   └── smtp.rs           # SMTP provider
│   │   ├── telegram/
│   │   │   ├── mod.rs
│   │   │   └── bot.rs            # Telegram Bot API
│   │   └── sms/
│   │       ├── mod.rs
│   │       └── twilio.rs         # Twilio SMS
│   │
│   ├── templates/                # Template rendering
│   │   ├── mod.rs
│   │   └── renderer.rs           # Tera template engine
│   │
│   ├── settings/                 # Site settings management
│   │   ├── mod.rs
│   │   ├── store.rs              # ValKey storage
│   │   └── models.rs             # Settings structs
│   │
│   ├── retry/                    # Retry/backoff logic
│   │   ├── mod.rs
│   │   └── manager.rs            # Exponential backoff
│   │
│   ├── filters/                  # Message filters
│   │   ├── mod.rs
│   │   └── spam.rs               # Basic spam filter
│   │
│   ├── crypto/                   # Encryption utilities
│   │   ├── mod.rs
│   │   └── secrets.rs            # AES-GCM for credentials
│   │
│   └── api/                      # Admin dashboard API
│       ├── mod.rs
│       ├── server.rs             # Axum server setup
│       ├── auth.rs               # Session authentication
│       ├── routes/
│       │   ├── mod.rs
│       │   ├── health.rs         # Health check
│       │   ├── sites.rs          # Site management
│       │   ├── messages.rs       # Message history
│       │   ├── templates.rs      # Template management
│       │   └── stats.rs          # Statistics
│       └── dashboard/
│           ├── mod.rs
│           └── templates/        # Htmx templates
│               ├── base.html
│               ├── dashboard.html
│               ├── sites.html
│               ├── messages.html
│               └── settings.html
│
└── tests/
    ├── integration/
    │   ├── consumer_test.rs
    │   └── channel_test.rs
    └── unit/
        ├── router_test.rs
        └── template_test.rs
```

---

## §10 INTEGRATION WITH GSD

```yaml
@STREAM_SUBSCRIPTION:
  # GSD-COMMS discovers sites from GSD site registry
  discovery_key: gsd:site:*:meta
  stream_pattern: {site_id}:gsd:comms:{environment}
  consumer_group: gsd_comms_dispatch

@ACL_USER:
  name: gsd_comms
  permissions:
    - +xreadgroup +xack +xpending +xclaim  # Stream consumer ops
    - +get +set +hget +hset +hdel          # Settings storage
    - +keys +scan                          # Discovery
    - ~*:gsd:comms:*                       # Comms streams
    - ~*:comms:config                      # Settings keys
    - ~*:comms:messages:*                  # Message state
    - ~gsd:site:*:meta                     # Site discovery (read)

@STATUS_UPDATES:
  # GSD-COMMS updates dispatch status in the original message
  # or stores in separate tracking key
  tracking_key: {site_id}:comms:messages:{message_id}
  fields:
    status: pending|processing|sent|failed|spam
    attempts: number
    channels_sent: ["email", "telegram"]
    channels_failed: ["sms"]
    last_error: string?
    sent_at: timestamp?

@SHARED_VALKEY:
  # Uses same ValKey instance as GSD
  # ACL isolation ensures separation
  host: 127.0.0.1
  port: 47445
```

---

## §11 SECURITY

```yaml
@CREDENTIAL_ENCRYPTION:
  algorithm: AES-256-GCM
  key_source: environment-variable (GSD_COMMS_ENCRYPTION_KEY)
  key_rotation: manual (with-re-encryption-script)
  storage_format: "encrypted:base64(nonce:ciphertext:tag)"

@ACL_ISOLATION:
  - separate-user-from-gsd_daemon
  - keyspace-restricted-to-comms-patterns
  - no-write-access-to-GSD-operational-keys

@RATE_LIMITING:
  - per-channel-limits (prevent-abuse)
  - per-site-limits (multi-tenant-fairness)
  - global-limits (system-protection)

@INPUT_VALIDATION:
  - sanitize-template-variables
  - validate-email-addresses
  - validate-phone-numbers
  - escape-markdown-in-telegram

@AUDIT_LOGGING:
  - log-all-dispatches
  - log-configuration-changes
  - log-authentication-events
```

---

## §12 FUTURE ROADMAP

```yaml
@PHASE_1: # MVP
  - stream-consumer
  - email-channel (SMTP)
  - basic-settings-storage
  - CLI-interface
  - simple-templates

@PHASE_2: # Multi-channel
  - telegram-channel
  - sms-channel (Twilio)
  - admin-dashboard (Htmx)
  - rate-limiting

@PHASE_3: # Production-ready
  - credential-encryption
  - retry-with-backoff
  - delivery-tracking
  - statistics

@PHASE_4: # Advanced
  - spam-filtering
  - ML-based-spam-detection
  - webhook-channel
  - Discord/Slack-channels
  - push-notifications

@PHASE_5: # Enterprise
  - multi-node-clustering
  - message-queuing (internal)
  - delivery-webhooks
  - SLA-monitoring
```

---

## §13 SEMANTIC ANCHORS

```yaml
@NOTIFICATION: email|telegram|sms|push|webhook|discord|slack
@STREAMING: XREADGROUP|consumer-groups|XACK|XCLAIM|PEL
@ROUTING: type-based|priority-based|settings-based|rule-engine
@RESILIENCE: retry|backoff|exponential|jitter|circuit-breaker
@TEMPLATES: tera|minijinja|html|markdown|plaintext
@SECURITY: AES-GCM|encryption|ACL|rate-limiting|input-validation
@DASHBOARD: htmx|axum|server-rendered|partial-updates
```

---

```yaml
@VALIDATION: design-phase(2026-01-10)|confidence:0.95
@RELATED: GSD(main-daemon)|GSD-Client(PHP)|GSD-BAK(ops)
```
