<p align="center">
  <a href="https://geodineum.com">
    <img src=".github/geodineum-logo.png" alt="Geodineum" width="128">
  </a>
</p>

# Geodineum-COMMS

The notification daemon of a Geodineum Constellation: it consumes per-site
message streams from ValKey and dispatches them to email, Telegram, and SMS,
archiving every message to SQLite.

Built by **Niels Erik Toren** · Rust daemon, crate `geodineum-comms` (see `Cargo.toml` for the version)

---

## What it is

Geodineum-COMMS is a stateless [tokio](https://tokio.rs) daemon and the
companion of `gNode` - both talk to the same ValKey instance (port 47445) under
separate ACL users. Services never call it directly: they `XADD` a message onto
their site's comms stream, and the daemon consumes it, routes it to the enabled
channels, and writes a durable record to a per-site SQLite archive.

It holds no message state of its own. A ValKey stream entry is acknowledged only
after its SQLite archive commits, so a restart resumes cleanly from the last
acknowledgement and nothing in flight is lost. The operator view lives in the
Constellation's wp-admin (Geodineum → Comms), not in this daemon.

## Public build surface

You integrate with COMMS by **producing a message**, not by calling an API. The
supported surface is the **comms message wire format** - the fields you `XADD`
onto `{site_id}:gnode:comms:{env}`. Any language that can write a ValKey stream
entry can drive it; the exact field table (scalars plus the JSON-string
`sender` / `content` / `metadata` / `dispatch` fields) is in
**[`CONTRACT.md`](CONTRACT.md)**, its single home.

Most producers never assemble the fields by hand - the **gNode-Client** library
(`queueCommsMessage` / `queueContactForm`) emits exactly this shape from PHP.

- **Operator surface** - the `geodineum-comms` binary and the `geodineum comms`
  manifest verbs (status, test-send, contract) inspect and exercise the daemon.
  Run `geodineum-comms --help` for the current subcommands; they are an operator
  tool, not a build target.
- **Internal** - everything under `src/` (the consumer, router, channels,
  templating, retry, persistence, and inbound modules) is implementation and may
  change without notice.

## Capabilities

- **Multi-channel dispatch** - email (SMTP), Telegram (Bot API), and SMS
  (Twilio), selected per message by type, priority, and per-site routing rules.
- **Durable archive with an acknowledgement fence** - every message is written
  to a per-site SQLite database; the ValKey entry is `XACK`ed only after that
  write succeeds, so ValKey and the archive never disagree.
- **Selective retry** - a channel that fails after acknowledgement is
  re-dispatched from stream history with exponential backoff, and only the
  failed channels are retried; routing and filters re-apply each time.
- **Non-production send gate** - only a `production` environment permits real
  sends; anything else is logged as a dry-run and archived, but no email, SMS, or
  Telegram is fired.
- **Zero-restart onboarding** - site streams are re-discovered on an interval, so
  a site added after the daemon starts is consumed without a restart.
- **Per-site configuration** - channels, recipients, priority/type filters, rate
  limits, and retention are configured per site in ValKey, independent of the
  daemon.
- **Two-way operator chat** - an inbound Telegram path turns operator replies
  into commands and workflow dispatches routed back to the originating component.

## Contract

The precise integration surface - every wire field, the stream and
settings-key layout, the DTAP send gate, the SQLite schema, and the public Rust
types - is in **[`CONTRACT.md`](CONTRACT.md)**. Agents should prime from
**[`CONTRACT.scn.md`](CONTRACT.scn.md)**. Print it on a host any time with
`geodineum comms contract`.

## Quick start

The Geodineum installer builds the binary, provisions the ACL user, and installs
the service; these commands assume a running daemon.

```php
// Produce a message the way most callers do - via gNode-Client, which emits the
// exact comms wire shape and injects the site's credentials.
use gCore\gNode\gNodeClient;

$client = gNodeClient::forSite('mysite', 'production');
$client->queueContactForm('Jane Doe', 'jane@example.com', 'Hello', 'Interested in a demo.');
```

```sh
# The daemon consumes, dispatches, and archives it. Inspect the result:
systemctl status geodineum-comms
geodineum-comms messages --site-id mysite --limit 5   # the archived message + its status
geodineum comms status                                # daemon state + per-stream counts
```

To produce the message without PHP, `XADD` it directly. The braces are a literal
ValKey cluster hash-tag - write `{mysite}`, not `mysite`:

```sh
# Authenticate as the producing site's ACL user (the same credential gNode-Client uses).
AUTH="$(sudo cat /etc/geodineum/credentials/mysite.password)"
REDISCLI_AUTH="$AUTH" redis-cli -p 47445 XADD '{mysite}:gnode:comms:production' '*' \
    id "$(uuidgen)" type contact timestamp "$(date -Iseconds)" \
    environment production priority 3 \
    content  '{"subject":"New contact","body":"Interested in a demo."}' \
    dispatch '{"channels":["email"],"status":"pending","attempts":0}'
```

The full field table and every optional field are in
[`CONTRACT.md`](CONTRACT.md).

## Limits worth knowing

- **Only `production` sends for real.** In any other environment the daemon
  archives the message and logs a dry-run line but fires nothing - start it with
  `--allow-nonprod-send` to override.
- **Per-site secrets are stored as plain values**, guarded by the ValKey ACL.
  There is no encryption at rest for the SQLite archive either; rely on
  filesystem-level protection. The `encrypt` CLI verb is a stub.
- **Some CLI verbs are stubs** - `stop` and `status` print a pointer to the
  systemd unit rather than acting on the daemon.
- **The inbound Telegram chat is beta** for this release.
- **SMS rate limiting is operator-configured** - there is no automatic
  carrier-feedback throttling.
- **Message `type` and dispatch `status` are not enum-validated on the wire**  - 
  any string is stored. Send the documented values.

## Collaborate

Contributions are welcome. Open issues and pick up work on the ecosystem board
at [geodineum.com](https://geodineum.com); issues tagged `good-first-issue` are
a good place to start.

- Fork, branch, and open a pull request against `main`.
- Any change to a wire contract must update **both** `CONTRACT.md` and
  `CONTRACT.scn.md` in the same commit.
- A change to a signed extension must be re-signed in the same commit.

## Author & support

Built by **Niels Erik Toren**.

If you want to support the work:

| Currency | Address |
|---|---|
| Bitcoin (BTC) | `bc1qwf78fjgapt2gcts4mwf3gnfkclvqgtlg4gpu4d` |
| Ethereum (ETH) | `0xf38b517Dd2005d93E0BDc1e9807665074c5eC731` / `nierto.eth` |
| Monero (XMR) | `8BPaSoq1pEJH4LgbGNQ92kFJA3oi2frE4igHvdP9Lz2giwhFo2VnNvGT8XABYasjtoVY2Qb3LVHv6CP3qwcJ8UnyRtjWRZ5` |

## Disclaimer

This software is provided **"as is"**, without warranty of any kind, express or
implied. Use of this software is entirely at your own risk. In no event shall the
author or contributors be held liable for any damages arising from the use or
inability to use this software.

## License

Licensed under either of

* [Apache License, Version 2.0](LICENSE-APACHE)
* [MIT License](LICENSE-MIT)

at your option.
