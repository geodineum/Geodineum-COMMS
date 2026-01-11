# GSD-COMMS

**Notification Daemon for GSD** - Dispatches notifications via email, Telegram, and SMS based on messages in GSD comms streams.

## Overview

GSD-COMMS is a Rust daemon that:

1. **Listens** to GSD comms streams (`{site_id}:gsd:comms:{environment}`) via consumer groups
2. **Routes** messages based on type, priority, and per-site configuration
3. **Dispatches** notifications through configurable channels (email, Telegram, SMS)
4. **Retries** failed deliveries with exponential backoff
5. **Provides** an admin dashboard for configuration and monitoring

## Quick Start

```bash
# Build
cargo build --release

# Run with default settings (requires ValKey connection)
./target/release/gsd-comms --redis-auth "$(cat .gsd/valkey_comms.password)" start

# Test channels for a site
./target/release/gsd-comms test --site-id staging_nierto_com --channel email
```

## Architecture

```
GSD Client (PHP)
       ↓ XADD
ValKey Comms Stream ({site_id}:gsd:comms:{env})
       ↓ XREADGROUP
GSD-COMMS Daemon
  ├── Stream Consumer (multi-site discovery)
  ├── Message Router (type/priority/settings)
  ├── Channel Providers
  │   ├── Email (SMTP via lettre)
  │   ├── Telegram (Bot API via reqwest)
  │   └── SMS (Twilio API via reqwest)
  ├── Template Engine (Tera)
  ├── Retry Manager (exponential backoff)
  └── Spam Filter (keyword/IP blocklists)
       ↓
Admin Dashboard (Axum + Htmx)
```

## Configuration

### Per-Site Settings

Each site stores its notification settings in ValKey at `{site_id}:comms:config`:

```yaml
site_id: staging_nierto_com
enabled: true

channels:
  email:
    enabled: true
    config:
      smtp_host: smtp.example.com
      smtp_port: 587
      smtp_user: notifications@example.com
      smtp_pass: "encrypted:..."
      from_email: notifications@example.com
      from_name: "My Site Notifications"
    recipients:
      - email: admin@example.com
        types: ["all"]
        min_priority: 3

  telegram:
    enabled: true
    config:
      bot_token: "encrypted:..."
      chat_id: "-1001234567890"
    recipients:
      - chat_id: "-1001234567890"
        types: ["alert", "error"]
        min_priority: 2

routing_rules:
  - type: contact
    channels: [email]
  - type: alert
    channels: [email, telegram, sms]
    priority_override: 1
```

### CLI Options

```
gsd-comms [OPTIONS] [COMMAND]

Options:
  --redis-host <HOST>    ValKey host [default: 127.0.0.1]
  --redis-port <PORT>    ValKey port [default: 47445]
  --redis-user <USER>    ACL username [default: gsd_comms]
  --redis-auth <AUTH>    ACL password
  --config <PATH>        Config file [default: config/default.yaml]
  --log-level <LEVEL>    Log level [default: info]
  --api-port <PORT>      Dashboard port [default: 8080]
  --environment <ENV>    DTAP environment [default: production]

Commands:
  start     Start the daemon
  stop      Stop the daemon
  status    Check daemon status
  test      Test notification channels
  encrypt   Encrypt a secret value
```

## Message Format

Messages in the comms stream follow this structure:

```json
{
  "id": "uuid",
  "type": "contact|alert|error|system",
  "timestamp": "ISO-8601",
  "site_id": "site_name",
  "priority": 1-5,
  "sender": {
    "name": "string",
    "email": "string",
    "phone": "string"
  },
  "content": {
    "subject": "string",
    "body": "string"
  },
  "dispatch": {
    "channels": ["email", "telegram"],
    "status": "pending"
  }
}
```

## Admin Dashboard

Access the dashboard at `http://localhost:8080/dashboard`:

- **Overview**: Statistics and recent messages
- **Sites**: Configure per-site notification settings
- **Messages**: View message history and retry failed deliveries
- **Settings**: Global configuration

## Channel Providers

### Email (SMTP)

Uses [lettre](https://github.com/lettre/lettre) for async SMTP with:
- Connection pooling
- TLS/STARTTLS support
- HTML + plaintext multipart emails

### Telegram

Uses the [Telegram Bot API](https://core.telegram.org/bots/api) directly via reqwest:
- MarkdownV2 formatting
- Channel and group posting
- Rate limiting (30 msg/sec)

### SMS (Twilio)

Uses the [Twilio REST API](https://www.twilio.com/docs/sms/api) via reqwest:
- International number support
- Message truncation (1600 char limit)
- Cost-controlled rate limiting (10 msg/min default)

## Integration with GSD

GSD-COMMS uses the same ValKey instance as GSD but with a separate ACL user:

```bash
# Create ACL user for GSD-COMMS
./scripts/create-acl-user.sh gsd_comms
```

Required permissions:
- `+xreadgroup +xack +xpending +xclaim` - Stream consumer operations
- `+get +set +hget +hset` - Settings storage
- `~*:gsd:comms:*` - Comms streams (all sites)
- `~*:comms:config` - Settings keys
- `~gsd:site:*:meta` - Site discovery (read-only)

## Directory Structure

```
GSD-COMMS/
├── Cargo.toml
├── CLAUDE.md              # LLM context document
├── config/
│   ├── default.yaml       # Default configuration
│   └── templates/         # Notification templates
│       ├── email/
│       ├── telegram/
│       └── sms/
├── scripts/
│   └── install-service.sh
└── src/
    ├── main.rs            # Entry point
    ├── lib.rs             # Library exports
    ├── api/               # Admin dashboard
    ├── channels/          # Notification providers
    ├── consumer/          # Stream consumer
    ├── filters/           # Spam detection
    ├── retry/             # Retry logic
    ├── router/            # Message routing
    ├── settings/          # Site configuration
    └── templates/         # Template rendering
```

## Development

```bash
# Build debug
cargo build

# Run tests
cargo test

# Run with debug logging
RUST_LOG=debug cargo run -- --redis-auth "..." start
```

## License

Same as GSD - see the main repository for details.
