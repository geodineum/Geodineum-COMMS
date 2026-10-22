# Changelog

All notable changes to this project are documented here. The format follows Keep a Changelog.

## [0.2.0] — unreleased

### Added

- Addressed messages: an optional `to` names the one person a message is for (email only), for the types a site allows (default `planning`), under a per-site daily cap.

### Breaking: the one topology

- Heartbeat key carries the node segment, in the constellation's family.
- ACL grants the heartbeat key in every constellation COMMS serves.
- Master delivery switch (`--delivery off`) makes every send impossible.

