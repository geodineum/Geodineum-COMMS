# Changelog

All notable changes to this project are documented here. The format follows Keep a Changelog.

## [0.2.0] — unreleased

### Added

- Addressed messages: an optional `to` names the one person a message is for (email only), for the types a site allows (default `planning`), under a per-site daily cap.
- Telegram linked to a web subscriber (the Geodine window): `/start <site>-<CODE>` links an account; its messages continue the subscriber's conversation as the site's own relayed `direct`; `/unlink`; `to: {telegram: <consumer>}` reaches the linked chat only. Non-admins pass the bot's gate only to link, unlink, or once linked.

### Breaking: the one topology

- Heartbeat key carries the node segment, in the constellation's family.
- ACL grants the heartbeat key in every constellation COMMS serves.
- Master delivery switch (`--delivery off`) makes every send impossible.

