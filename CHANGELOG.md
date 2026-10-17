# Changelog

All notable changes to this project are documented here. The format follows Keep a Changelog.

## [0.2.0] — unreleased

### Breaking: the one topology

- Heartbeat key carries the node segment, in the constellation's family.
- ACL grants the heartbeat key in every constellation COMMS serves.
- Master delivery switch (`--delivery off`) makes every send impossible.

