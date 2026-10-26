# Changelog

All notable changes to this project are documented here. The format follows Keep a Changelog.

## [0.2.0] — unreleased

### Added

- Addressed messages: an optional `to` names the one person a message is for (email only), for the types a site allows (default `planning`), under a per-site daily cap.
- The failsafe: every email's Message-ID names its COMMS message; the archive records each channel's result (an
  email's `queued as`); `watch` (hourly timer) alerts on Telegram, straight from the bot, on a missing heartbeat and on
  every failed, partly sent or skipped delivery of a production site, mails a daily summary through COMMS and reads it
  back from the mail log (signed, delivered); `ledger <site>` shows what a site's mail did. Fixed: the archive's channel
  results never read back (0/1 against a boolean), and were never written.
- Telegram linked to a web subscriber (the Geodine window): `/start <site>-<CODE>` links an account; its messages continue the subscriber's conversation as the site's own relayed `direct`; `/unlink`; `to: {telegram: <consumer>}` reaches the linked chat only. Non-admins pass the bot's gate only to link, unlink, or once linked.

### Breaking: the one topology

- Heartbeat key carries the node segment, in the constellation's family.
- ACL grants the heartbeat key in every constellation COMMS serves.
- Master delivery switch (`--delivery off`) makes every send impossible.

